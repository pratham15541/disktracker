# Contributing

Thanks for contributing to DiskTracker.

## Development setup

- Install stable Rust (rustup recommended).
- Build the workspace:

```bash
cargo build
```

## Testing

Run the full suite:

```bash
cargo test
```

Target a specific crate:

```bash
cargo test -p disktracker-cli
cargo test -p disktracker-core
cargo test -p disktracker-db
cargo test -p disktracker-watch
cargo test -p disktracker-events
```

The CLI end-to-end tests run the `disktracker` binary via `assert_cmd` and use temporary directories and databases.

## Code style

- Keep changes focused and small.
- Prefer clear, descriptive names over cleverness.
- Run `cargo fmt` and `cargo clippy` when it helps, but avoid large reformat-only changes unless asked.

## Reporting issues

Include the command you ran, the platform, and any relevant logs or error output.
