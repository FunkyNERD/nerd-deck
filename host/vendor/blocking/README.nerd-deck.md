# blocking 1.7.0, patched

A copy of [blocking](https://crates.io/crates/blocking) 1.7.0 with one change,
in `Executor::main_loop` (src/lib.rs, marked `nerd-deck:`): an idle pool
thread exits after its 500 ms wait, as the comment there says it should.
Upstream only lets it exit when the pool is over its thread limit, so the
last thread wakes twice a second for the life of the process. zbus starts the
pool for every D-Bus connection, so the service, the tray and the settings
window all had one.

Drop this copy, and the `[patch.crates-io]` entry in host/Cargo.toml, once a
release of blocking lets idle threads exit.
