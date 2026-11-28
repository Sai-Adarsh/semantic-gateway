# Semantic inference gateway

[Quick start](../README.md). Run commands from the repository root.

This Rust HTTP gateway reuses chat completions for repeat prompts and close embedding matches.

## Local run

```sh
cargo run --locked -- --demo
```

```sh
curl -sS http://127.0.0.1:8090/generate -H 'Content-Type: application/json' \
  -d '{"prompt":"Summarize the project status."}'
curl -sS http://127.0.0.1:8090/generate -H 'Content-Type: application/json' \
  -d '{"prompt":"Give me a summary of the project status."}'
curl -sS http://127.0.0.1:8090/generate -H 'Content-Type: application/json' \
  -d '{"prompt":"How does cell division work?"}'
curl -sS http://127.0.0.1:8090/metrics
```

The demo uses lexical vectors and echo responses. The first two prompts map to the same vector. The three requests yield a miss, a hit, then a miss. On a fresh instance, `semantic_generation_calls_total` records two calls.

## Real embeddings and completions

Set the model IDs and full upstream URLs. The gateway uses the OpenAI-compatible interfaces for [embeddings](https://developers.openai.com/api/reference/resources/embeddings/methods/create) and [chat completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).

```sh
export EMBEDDING_URL=http://127.0.0.1:8081/v1/embeddings EMBEDDING_MODEL=embedding-model
export GENERATION_URL=http://127.0.0.1:8082/v1/chat/completions GENERATION_MODEL=inference-model
# Set GATEWAY_API_KEY in the environment and any required upstream keys.
cargo run --locked
```

Send the gateway key as a bearer token to `/generate` and `/metrics`. Upstream keys are optional and use `EMBEDDING_API_KEY` and `GENERATION_API_KEY`. Use `CACHE_TTL_SECONDS`, `CACHE_CAPACITY`, `CACHE_SIMILARITY` and `LISTEN` to override the defaults.

Prompts are limited to 8192 UTF-8 bytes after JSON decoding. The HTTP request body is limited to 64 KiB.

Embedding and generation response bodies are limited to 1 MiB each. The gateway returns HTTP 502 when an upstream response exceeds that limit.

Each process serves one tenant and one generation policy. Use separate instances for different authorization scopes.

## Cache behavior

Entries expire after ten minutes by default. When the cache is full, it evicts the least recently used entry. The gateway stores each generated answer under its original prompt.

- **Exact hit.** A stored prompt match skips embedding and returns `similarity: null` with `cache_hit: true`.
- **Semantic lookup.** On an exact miss, the gateway embeds the prompt and returns the best cosine match if it exceeds 0.98 by default. Otherwise it calls the generation endpoint and caches the result. A semantic hit never creates an exact entry for the matching query.
- **Exact-only request.** Send `"exact_only":true` to cache repeated prompt text. On a miss, the gateway skips embedding and cosine lookup. An exact-only entry is eligible for its original prompt.
- **Uncached request.** Send `"cacheable":false` for a fresh answer. The gateway bypasses embedding and caching.

Cosine similarity can match prompts that require different answers. Measure the false-hit rate on representative prompt pairs and include the cost of embedding calls when estimating savings.

## Shared requests and overload

Concurrent misses for the same prompt and cache mode share an in-flight request. Exact-only requests have their own group so they cannot inherit a semantic candidate. If both groups generate for the same prompt, the cache keeps the first stored answer and can add the semantic embedding to that entry. Work continues if the originating client disconnects. A response with `coalesced: true` joined an active request.

| Setting | Limit |
| --- | --- |
| `CACHE_MAX_INFLIGHT` | Upstream work across cacheable groups and individual uncached requests. Exact hits and followers that join an existing group use no extra slots. |
| `CACHE_MAX_WAITERS_PER_KEY` | Followers per group. |

Both limits default to 64. Requests above either limit receive HTTP 503 with `Retry-After: 1`.

`semantic_inflight_keys` counts active cacheable groups. `semantic_inflight_loads` counts those groups and individual uncached requests. Counters track embedding, generation, hits, misses, coalescing, and overload.
