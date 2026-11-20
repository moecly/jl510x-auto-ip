#include "auto_ip.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void format_ipv4(uint32_t network_order, char out[16]) {
    uint32_t ip = ((network_order & 0xffu) << 24) |
                  ((network_order & 0xff00u) << 8) |
                  ((network_order & 0xff0000u) >> 8) |
                  ((network_order & 0xff000000u) >> 24);
    snprintf(out, 16, "%u.%u.%u.%u",
             (unsigned)((ip >> 24) & 0xffu),
             (unsigned)((ip >> 16) & 0xffu),
             (unsigned)((ip >> 8) & 0xffu),
             (unsigned)(ip & 0xffu));
}

static const char *status_name(uint8_t status) {
    switch (status) {
        case 0: return "applied";
        case 1: return "unchanged";
        case 2: return "save-failed";
        case 3: return "rejected";
        case 4: return "pending";
        default: return "unknown";
    }
}

static void print_interfaces(const auto_ip_interface_list *list) {
    size_t count = auto_ip_interface_list_count(list);
    for (size_t i = 0; i < count; i++) {
        auto_ip_interface iface;
        if (auto_ip_interface_list_get(list, i, &iface) != 0) {
            continue;
        }
        printf("[%zu] %s\n", i, iface.name);
        printf("    %s%s\n", iface.description, iface.is_up ? " [up]" : " [down]");
        if (iface.has_mac) {
            printf("    mac %02x:%02x:%02x:%02x:%02x:%02x\n",
                   iface.mac[0], iface.mac[1], iface.mac[2],
                   iface.mac[3], iface.mac[4], iface.mac[5]);
        }
    }
}

static int get_interface(const auto_ip_interface_list *list, size_t index,
                         auto_ip_interface *out) {
    if (auto_ip_interface_list_get(list, index, out) != 0) {
        fprintf(stderr, "invalid interface index: %zu\n", index);
        return -1;
    }
    return 0;
}

static int find_interface(const auto_ip_interface_list *list, const char *name,
                          auto_ip_interface *out) {
    size_t count = auto_ip_interface_list_count(list);
    for (size_t i = 0; i < count; i++) {
        auto_ip_interface iface;
        if (auto_ip_interface_list_get(list, i, &iface) == 0 &&
            strcmp(iface.name, name) == 0) {
            *out = iface;
            return 0;
        }
    }
    return -1;
}

static int select_interface(const auto_ip_interface_list *list,
                            const char *requested, auto_ip_interface *out) {
    size_t count = auto_ip_interface_list_count(list);
    if (count == 0) {
        fprintf(stderr, "no interfaces found\n");
        return -1;
    }

    if (requested != NULL) {
        char *end = NULL;
        long index = strtol(requested, &end, 10);
        if (end != requested && *end == '\0' &&
            index >= 0 && (size_t)index < count) {
            return get_interface(list, (size_t)index, out);
        }
        if (find_interface(list, requested, out) == 0) {
            return 0;
        }
        fprintf(stderr, "interface not found: %s\n", requested);
        return -1;
    }

    char line[32];
    printf("select interface index [0-%zu] (default 0): ", count - 1);
    fflush(stdout);
    if (fgets(line, sizeof(line), stdin) == NULL || line[0] == '\n') {
        return get_interface(list, 0, out);
    }

    char *end = NULL;
    long index = strtol(line, &end, 10);
    if (end == line || (*end != '\0' && *end != '\n') ||
        index < 0 || (size_t)index >= count) {
        fprintf(stderr, "invalid selection\n");
        return -1;
    }
    return get_interface(list, (size_t)index, out);
}

int main(int argc, char **argv) {
    auto_ip_interface_list *list = NULL;
    char *error = NULL;

    if (auto_ip_list_interfaces(&list, &error) != 0) {
        fprintf(stderr, "scan failed: %s\n", error ? error : "unknown error");
        auto_ip_string_free(error);
        return 1;
    }

    print_interfaces(list);
    if (argc > 1 && strcmp(argv[1], "--list") == 0) {
        auto_ip_interface_list_free(list);
        return 0;
    }

    auto_ip_interface iface;
    const char *requested = argc > 1 ? argv[1] : NULL;
    if (select_interface(list, requested, &iface) != 0) {
        auto_ip_interface_list_free(list);
        return 1;
    }

    auto_ip_config cfg;
    auto_ip_config_init(&cfg);
    cfg.interface_name = iface.name; /* iface points into list, keep list alive */
    if (argc > 2) {
        cfg.base_ip = argv[2];
    }

    auto_ip_result *result = NULL;
    printf("\nusing %s, base ip %s\n", iface.name, cfg.base_ip);
    if (auto_ip_run(&cfg, &result, &error) != 0) {
        fprintf(stderr, "run failed: %s\n", error ? error : "unknown error");
        auto_ip_string_free(error);
        auto_ip_interface_list_free(list);
        return 1;
    }

    printf("sent: %u, replies: %zu, boards: %zu\n",
           auto_ip_result_sent(result),
           auto_ip_result_reply_count(result),
           auto_ip_result_board_count(result));

    size_t boards = auto_ip_result_board_count(result);
    if (boards == 0) {
        printf("no replies received\n");
    }
    for (size_t i = 0; i < boards; i++) {
        auto_ip_reply reply;
        char old_ip[16];
        char new_ip[16];
        if (auto_ip_result_board(result, i, &reply) != 0) {
            continue;
        }
        format_ipv4(reply.old_ip, old_ip);
        format_ipv4(reply.new_ip, new_ip);
        printf("board %-3u old %-15s -> new %-15s  ttl=%u  status=%s  reason=%u\n",
               reply.index, old_ip, new_ip, reply.ttl,
               status_name(reply.status), reply.reason);
    }

    auto_ip_result_free(result);
    auto_ip_interface_list_free(list);
    return 0;
}
