# Building and running

How to build, run and check the desktop app, and the toolchain it needs.

## Build and run

```powershell
cargo xtask build          # release build, copies MarqueePlayer.exe (Linux, macOS: MarqueePlayer) next to Cargo.toml
cargo xtask build --test   # the tests (release profile) first
cargo xtask check          # the checks before a push (below)
cargo xtask bench          # every bench mode (docs/testing.md, "The bench runner")
cargo run -p marquee-player --release   # run from source
cargo build -p marquee-player --release --features bench   # with the bench harness (MP_BENCH)
cargo xtask release        # a numbered release in dist/: on Windows the portable zip and the
                           # installer, on Linux (WSL) the AppImage, on macOS the disk image
                           # (docs/releases.md)
```

The scripted bench and measurement harness (`MP_BENCH`, [Measurement and test hooks](testing.md#measurement-and-test-hooks)) is a
cargo feature, `bench`, off by default: `cargo xtask build` and `cargo xtask release`
build without it; `cargo xtask bench` builds with it. By hand, add `--features bench` (from the workspace root,
`--features marquee-player/bench` with `--workspace`). Without it, `MP_BENCH` logs one line
saying the build has no bench harness and the app runs normally; the other development
variables (`MP_DATA_DIR`, `MP_MUSIC`, `MP_VOLUME`, `MP_WINDOW`) work in every build; the API
overrides (`MP_LASTFM_API`, `MP_LISTENBRAINZ_API`, `MP_LRCLIB_API`) only with the harness or in tests.

Needs the MSVC Rust toolchain and CMake (libopus is compiled from source by
`opusic-sys`). Tests that generate audio fixtures need `ffmpeg` on PATH and skip
without it.

On Linux the same commands build and run it (`cargo build -p marquee-player --release`, the
binary is `target/release/MarqueePlayer`); prerequisites, what differs and how it was tested
are in [Linux](linux.md#linux). On macOS too, with the Xcode command line tools and CMake: see
[macOS](macos.md#macos) (written without a Mac: not yet built or run on one).

`cargo xtask` (`crates/xtask`, an alias in `.cargo/config.toml`; standard library only, so it
compiles in seconds) is the one way to run the development commands on every system;
`cargo xtask --help`, and `--help` after a command, list the options. Every command builds in
`--target-dir DIR` if given, else in `CARGO_TARGET_DIR`, else in cargo's default `target/`
(the bench builds then in `target/bench`, so that they and the release build, with other
features, do not rebuild each other); `-j N` limits cargo's jobs.

### Checks before a push

`cargo xtask check` (Windows, Linux, WSL, macOS) runs what a push must pass, stopping
at the first failure with a line for each step and its time: `git diff --check` over the commits
since the branch left `master` (trailing spaces, a space before a tab, conflict markers;
`--base REV` for another start; android/ left out), `cargo fmt --check`, clippy over the
workspace with `-D warnings` without and with the bench harness, and `cargo test --workspace`,
all on the `fast` profile (in `target/fast`, or the target folder chosen, as above). The tree
has never been run through rustfmt (its lines are longer than any rustfmt setting keeps), so
the formatting step only says how many places rustfmt would change; `--strict-format` (or
`MP_CHECK_FMT=strict`) makes it fail the run. `cargo xtask release` runs its own tests and clippy on the release profile
(see [Version and releases](releases.md#version-and-releases)); these are the quicker ones for every push. Nothing runs on GitHub for a
push or a pull request: run the command yourself. GitHub Actions runs the same command
(`.github/workflows/desktop.yml`, on Windows, Ubuntu 24.04 and `macos-latest`, Apple Silicon)
only when a version is tagged, as the release run's first job on the systems it builds, and
by hand (Actions > Desktop > Run workflow, choosing the systems: all three, macOS alone,
Windows and Linux, or one of them).

The check also runs where git will not read the checkout as is: as root in WSL on the
Windows-side checkout git refuses a folder another user owns ("dubious ownership"), and the
check then trusts that one checkout for its own git calls (`git -c safe.directory=<checkout>`
on the command line: nobody's git config changes) and says so. Where git cannot read it at all
(no git, or a worktree whose `.git` names a Windows path), it says why and skips only the
whitespace step: check that where git works, or in a clone of its own.

As a git pre-push hook, opt-in and per clone: `cargo xtask check --install-hook` writes
`pre-push` (a short sh script that runs `cargo xtask check --hook`) into the clone's hooks
folder (`git rev-parse --git-path hooks`, shared by its worktrees; nothing in the git config
changes), and refuses to replace a hook that is there (`--force` replaces it). The hook runs the checks on the commits being
pushed; `git push --no-verify` skips it once, and deleting the file uninstalls it.

On first start, add a music folder or connect a music server (Settings > Library, or the
two buttons on the empty home page, Add folder and Connect a server).
Music folders are only ever read, unless the user turns on tag editing (Settings > Library >
Edit tags; see [Tag editing](features/metadata.md#tag-editing)).

## Slint

Slint is pinned to a master commit (`Cargo.toml`, workspace dependencies) for
`backdrop-blur`, which arrived after 1.18.1 and draws with the Skia renderer the app uses
(at this commit FemtoVG draws it too; the software renderer does not). The commit is past
the 1.19 pre-release branch (its crates say 1.20.0), which has `backdrop-blur` and the
window icon fix the app needs: move to the 1.19 release when it ships (replace the
`git`/`rev` pair with the version), checking the app on it. The
first build downloads Skia's prebuilt binaries (needs network once; about 6 minutes).
