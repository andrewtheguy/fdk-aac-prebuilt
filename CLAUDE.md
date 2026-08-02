# CLAUDE.md

## Running `./test-docker.sh`

The script needs a working Docker daemon, and the machine holding this checkout is not
necessarily the machine that has one. How Docker is reached varies per developer, so it is
deliberately not recorded here.

Before running `./test-docker.sh`, or before advising on a failure from it, look for
`CLAUDE.local.md` in the repository root and read it. It is gitignored, it is per-checkout,
and where it exists it is the authority on how this particular machine gets to Docker.

Treat whatever it says as given. Do not assume it describes a local daemon, a remote one, any
particular host, or any particular way of invoking the script — read it and follow it.

If there is no `CLAUDE.local.md`, or it says nothing about Docker, ask rather than guessing. A
bare `docker` on `PATH` is a guess like any other, and the failure it produces when wrong is
not always obvious as a connectivity problem.
