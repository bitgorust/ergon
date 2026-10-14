/* gapspin <cpu> <threshold us> [seconds]: spins on <cpu> reading the TSC and
 * prints every gap of at least <threshold us> as "<CLOCK_REALTIME s.ns> <gap
 * us>", the gap ending at that time: a stretch the CPU did not run this loop
 * (a guest preemption, which a trace of the CPU shows, or the hypervisor's,
 * which nothing in the guest does). Runs for [seconds], else until killed.
 * vm.sh preflight and diag-start run it. */
#define _GNU_SOURCE
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <x86intrin.h>

static double ns_per_tick(void)
{
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC_RAW, &a);
    const uint64_t t0 = __rdtsc();
    do
    {
        clock_gettime(CLOCK_MONOTONIC_RAW, &b);
    } while ((b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec) < 200000000L);
    const uint64_t t1 = __rdtsc();
    return ((b.tv_sec - a.tv_sec) * 1e9 + (b.tv_nsec - a.tv_nsec)) / (double)(t1 - t0);
}

int main(int argc, char **argv)
{
    if (argc < 3)
    {
        fprintf(stderr, "gapspin <cpu> <threshold us> [seconds]\n");
        return 2;
    }
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(atoi(argv[1]), &set);
    if (0 != sched_setaffinity(0, sizeof(set), &set))
    {
        perror("sched_setaffinity");
        return 1;
    }
    const double npt = ns_per_tick();
    const uint64_t threshold = (uint64_t)(atof(argv[2]) * 1000.0 / npt);
    const uint64_t end = argc > 3 ? (uint64_t)(atof(argv[3]) * 1e9 / npt) : UINT64_MAX;
    setvbuf(stdout, NULL, _IOLBF, 0);
    printf("# cpu=%s ns_per_tick=%.6f threshold_us=%s\n", argv[1], npt, argv[2]);
    const uint64_t start = __rdtsc();
    uint64_t last = start;
    for (;;)
    {
        const uint64_t now = __rdtsc();
        if (now - last >= threshold)
        {
            struct timespec rt;
            clock_gettime(CLOCK_REALTIME, &rt);
            printf("%ld.%09ld %.1f\n", (long)rt.tv_sec, rt.tv_nsec, (double)(now - last) * npt / 1000.0);
        }
        last = now;
        if (now - start >= end)
        {
            return 0;
        }
    }
}
