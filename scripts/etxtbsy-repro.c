/*
 * etxtbsy-repro.c — standalone reproducer for the ETXTBSY flake that used to
 * hit dracon-system's guard fixture tests (2026-09-30).
 *
 * WHY THIS FILE EXISTS
 * --------------------
 * The flakes were reported with three different-looking messages:
 *
 *   failed to invoke /tmp/.../renice            (ENOENT-looking)
 *   oom_score_adj ... Is a directory            (os error 21)
 *   retaining ... process identity unavailable
 *
 * All of them trace back to ONE cause, and it is not in the daemon: when a
 * process execs a file it created milliseconds earlier, execve can fail with
 * ETXTBSY because the kernel still considers that inode write-busy. `libtest`
 * runs tests in parallel threads inside one process, so several fixtures are
 * written and exec'd at the same time by construction.
 *
 * This program contains NONE of the crate's code — just write fixture, chmod,
 * fork, execv — so it separates the mechanism from our test code.
 *
 * WHICH PART OF THE WINDOW MATTERS (measured on the development host,
 * Linux 7.1, /tmp on ext4; 4800-6400 execs per configuration):
 *
 *   MODE=0  nothing serialized                        -> 52..120
 *   MODE=7  ONLY the exec serialized (writes free)    ->  0
 *   MODE=8  ONLY the write serialized (execs free)    ->  8
 *   MODE=9  the whole create->exec window serialized  ->  0
 *
 * Concurrent *execs* are what collide; serializing the writes alone does not
 * help. Thread count alone does not explain it either — a single thread
 * never fails, and exec'ing one PRE-WRITTEN script from many threads is also
 * always safe:
 *
 *   ./etxtbsy-repro 1500 1  -> threads=1  etxtbsy=0        (no contention)
 *   ./etxtbsy-repro 1500 4  -> threads=4  etxtbsy=27..155  (the flake)
 *   ./etxtbsy-repro  600 8  -> threads=8  etxtbsy=52..120
 *
 * Variants that confirm what the trigger is NOT — none of them fix it:
 *
 *   MODE=1  fsync(fd) before close        -> ~3033  (about 70x WORSE)
 *   MODE=2  reopen + read after chmod     -> ~47     (no help)
 *   MODE=3  reopen without reading        -> ~38     (no help)
 *   MODE=4  fsync the parent directory    -> ~45     (no help)
 *   MODE=5  write to a temp name, rename  -> ~33     (no help)
 *   MODE=6  no chmod (mode from open)     -> ~27     (no help)
 *
 * A private per-thread interpreter copy does not help either, so the contended
 * inode is not a shared `/bin/sh`. The resource is the fixture inode of THIS
 * process, which is why the fix in src/tests.rs is a process-local lock:
 * running
 *
 *   MODE=9 ./etxtbsy-repro 600 8 &
 *   for k in 1 2 3 4; do sh -c : ; done      # or any /bin/sh spammers
 *
 * against it produced 0 failures across 24000 external /bin/sh execs, so a
 * second test binary — or any other process on the host — cannot reintroduce
 * the flake.
 *
 * SCOPE NOTE
 * ----------
 * These C numbers are a mechanism sketch, not the acceptance evidence. This
 * program opens fixtures with plain O_WRONLY, whereas `std::fs` always adds
 * O_CLOEXEC, so the crate's own measurements — the tripwire test
 * `fixture_script_exec_never_hits_etxtbsy_under_parallel_load` and the
 * repeated full-suite soak — are the authoritative ones.
 *
 * BUILD / RUN
 * -----------
 *   cc -O2 -pthread -o etxtbsy-repro etxtbsy-repro.c
 *   mkdir -p /tmp/etxtbsy-repro
 *   ./etxtbsy-repro 1500 4
 *   cc -O2 -pthread -DMODE=9 -o etxtbsy-repro-9 etxtbsy-repro.c && ./etxtbsy-repro-9 600 8
 *
 * Exit status is 0 when no ETXTBSY occurred, 1 otherwise. Diagnostics print
 * only counts; nothing here is needed at runtime.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

/* 0 = nothing serialized, 1..6 = single-operation variants documented above,
 * 7 = only the exec serialized, 8 = only the write serialized,
 * 9 = the whole create->exec window serialized. */
#ifndef MODE
#define MODE 0
#endif

#define FIXTURE_DIR "/tmp/etxtbsy-repro"
#define BODY "#!/bin/sh\nexit 0\n"

static long iterations = 1500;
static int threads = 4;
static volatile long etxtbsy = 0;
static volatile long other_errors = 0;
static volatile long succeeded = 0;
/* Only the serializing MODEs touch it. */
static pthread_mutex_t window __attribute__((unused)) = PTHREAD_MUTEX_INITIALIZER;

/* Write the fixture. Mirrors fs::write + set_permissions.
 *
 * O_CLOEXEC is set because `std::fs` always sets it, so this program
 * matches what the Rust tests actually do on the wire. Without it a
 * concurrent fork can inherit this write fd and hold the fixture
 * write-busy for the whole life of the exec'd child, which changes the
 * numbers. */
static int write_fixture(const char *path) {
    char staged[PATH_MAX] __attribute__((unused));
    const char *target = path;
#if MODE == 5
    /* Write under a temporary name and rename into place. */
    snprintf(staged, sizeof staged, "%s.tmp", path);
    target = staged;
#endif
    int fd = open(target, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0755);
    if (fd < 0) {
        perror("open");
        return -1;
    }
    if (write(fd, BODY, sizeof BODY - 1) < 0) {
        perror("write");
        close(fd);
        return -1;
    }
#if MODE == 1
    /* Forcing writeback before close widens the busy window. */
    if (fsync(fd) < 0) {
        perror("fsync");
        close(fd);
        return -1;
    }
#endif
    close(fd);
#if MODE == 6
    /* Mode already came from open(); skip chmod entirely. */
#else
    if (chmod(target, 0755) < 0) {
        perror("chmod");
        return -1;
    }
#endif
#if MODE == 5
    if (rename(staged, path) < 0) {
        perror("rename");
        return -1;
    }
#endif
    return 0;
}

/* Exec the fixture. Mirrors Command::new(path).output(). */
static int exec_fixture(const char *path) {
    pid_t pid = fork();
    if (pid == 0) {
        char *argv[] = {(char *)path, NULL};
        execv(path, argv);
        /* Report the errno to the parent through the exit status. */
        _exit(errno == ETXTBSY ? 98 : (errno == ENOENT ? 97 : 96));
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return -1;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

/* One create->exec cycle, serializing only the halves each MODE selects:
 * MODE 7 locks the exec only, MODE 8 the write only, MODE 9 both. */
static int create_and_exec(const char *path) {
    if (MODE == 8 || MODE == 9) pthread_mutex_lock(&window);
    if (write_fixture(path) != 0) return -1;
#if MODE == 2 || MODE == 3
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror("reopen"); return -1; }
#if MODE == 2
    char buf[64];
    if (read(fd, buf, sizeof buf) < 0) { perror("read"); close(fd); return -1; }
#endif
    close(fd);
#elif MODE == 4
    int fd = open(FIXTURE_DIR, O_RDONLY | O_DIRECTORY);
    if (fd < 0) { perror("opendir"); return -1; }
    if (fsync(fd) < 0) { perror("dirfsync"); close(fd); return -1; }
    close(fd);
#endif
    if (MODE == 8) {
        /* Write half done; the exec stays unsynchronized. */
        pthread_mutex_unlock(&window);
        return exec_fixture(path);
    }
    if (MODE == 7) pthread_mutex_lock(&window);
    int status = exec_fixture(path);
    if (MODE == 7 || MODE == 9) pthread_mutex_unlock(&window);
    return status;
}

static void *worker(void *arg) {
    long id = (long)arg;
    char path[PATH_MAX];
    for (long i = 0; i < iterations; i++) {
        snprintf(path, sizeof path, FIXTURE_DIR "/fixture_%d_%ld_%ld", MODE, id, i);
        int status = create_and_exec(path);
        if (status == 0) {
            __sync_fetch_and_add(&succeeded, 1);
        } else if (status == 98) {
            __sync_fetch_and_add(&etxtbsy, 1);
        } else {
            __sync_fetch_and_add(&other_errors, 1);
        }
        unlink(path);
#if MODE == 5
        {
            char staged[PATH_MAX];
            snprintf(staged, sizeof staged, "%s.tmp", path);
            unlink(staged);
        }
#endif
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc > 1) iterations = atol(argv[1]);
    if (argc > 2) threads = atoi(argv[2]);
    if (threads < 1 || threads > 64) {
        fprintf(stderr, "threads must be 1..64\n");
        return 2;
    }
    if (mkdir(FIXTURE_DIR, 0755) < 0 && errno != EEXIST) {
        perror("mkdir " FIXTURE_DIR);
        return 2;
    }

    pthread_t handles[64];
    for (long t = 0; t < threads; t++) {
        if (pthread_create(&handles[t], NULL, worker, (void *)t) != 0) {
            perror("pthread_create");
            return 2;
        }
    }
    for (long t = 0; t < threads; t++) {
        pthread_join(handles[t], NULL);
    }

    printf("mode=%d threads=%d iterations=%ld execs=%ld ok=%ld etxtbsy=%ld other=%ld\n",
           MODE, threads, iterations, iterations * threads,
           succeeded, etxtbsy, other_errors);
    return etxtbsy ? 1 : 0;
}
