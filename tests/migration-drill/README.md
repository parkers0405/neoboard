# Offline-copy migration drill

On POSIX with Python 3.8+ and two trusted, API-compatible `neochatd` builds, run from the repository root:

```sh
python3 tests/migration-drill/drill.py --old /absolute/path/to/legacy/neochatd --new /absolute/path/to/candidate/neochatd
```

Supply executables, **never a board directory, endpoint, or credential**. The script does not build software, search `PATH` for daemons, or use CLI discovery. Each child gets a separate private temporary data directory and home, a clean environment, and an ephemeral IPv4 loopback port. HTTP requests go directly to the port in that child's fresh endpoint record, without proxies or redirects. The script stops its own children and removes both directories on normal exit or a failed check.

The old daemon creates a public thread and reply, a labeled private task with a binary attachment, and a channel-scoped member credential. The drill checks the fixture through HTTP, stops the old process cleanly, then copies `board.sqlite3`, any WAL or rollback journal, and the host/admin token files. It does not copy discovery, locks, sharing state, or derived shared memory. On the new daemon it repeats the checks using the **old credentials and IDs**: full host message records, reply linkage, task metadata, attachment listing and exact bytes, member visibility and denied private downloads/listings, and rejection of an invalid credential. A mismatch or startup/shutdown failure exits nonzero. This is an offline migration fixture, not a backup recipe or crash-recovery test.

A pass demonstrates compatibility for the two SHA-256 fingerprints printed. Running with the same executable twice, or two SQLite builds, is a **harness smoke test only**. For SQLite-to-Turso evidence, independently establish that `--old` was built with the legacy SQLite engine and `--new` with the Turso storage implementation, and retain build provenance alongside the output. Neither a binary's filename nor the current checkout's manifest establishes how an existing executable was built.

## Local verification

| Old → new | Result | Interpretation |
| --- | --- | --- |
| `target/release/neochatd` → same executable | PASS | Harness smoke only. |
| `target/release/neochatd` → `target/debug/neochatd` | PASS | The legacy and Turso-linked binaries below passed the copied-fixture drill. This is evidence for this fixture, not every production database. |

The release SHA-256 was `9f4b67a344f19794e74c6aade14000f711759b4e45ffdbc728320a47a5d7bf90` (`nm` showed `rusqlite` symbols, not `turso_core`); the passing debug SHA-256 was `dc656474c984e6cb5067d0ebef8ed1679f117d3ca6c52b05e32b5071b8d0319f` (`turso_core` symbols, not `rusqlite`). Recheck hashes and linkage before attributing these results to later builds at the same paths.