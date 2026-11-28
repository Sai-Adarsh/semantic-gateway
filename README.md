# Semantic inference gateway

## What it is

This Rust HTTP gateway reuses chat completions for repeat prompts and close embedding matches.

## How to run

Start the demo from the repo root.

```sh
cargo run --locked -- --demo
```

Send both requests from another terminal. Expect a cache miss, then a hit.

```sh
curl -sS http://127.0.0.1:8090/generate -H 'Content-Type: application/json' \
  -d '{"prompt":"Summarize the project status."}'
curl -sS http://127.0.0.1:8090/generate -H 'Content-Type: application/json' \
  -d '{"prompt":"Give me a summary of the project status."}'
```

## Other things

[Gateway reference](docs/reference.md)
