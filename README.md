# Semantic inference gateway

This Rust HTTP gateway returns a cached completion when the best cosine match for a prompt's embedding exceeds 0.98. Otherwise it calls the generation endpoint and stores the result. Entries expire after ten minutes by default. A full cache evicts its least recently used entry.

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

The demo uses lexical vectors and echo responses. The first two prompts map to the same vector. Expect a miss, a hit, then a miss, with two calls recorded by `semantic_generation_calls_total`.

## Real embeddings and completions

Set the model IDs and full URLs for the two upstream endpoints. They use the OpenAI-compatible [embeddings](https://developers.openai.com/api/reference/resources/embeddings/methods/create) and [chat completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create) interfaces.

```sh
export EMBEDDING_URL=http://127.0.0.1:8081/v1/embeddings EMBEDDING_MODEL=embedding-model
export GENERATION_URL=http://127.0.0.1:8082/v1/chat/completions GENERATION_MODEL=inference-model
# Set GATEWAY_API_KEY in the environment and any required upstream keys.
cargo run --locked
```

Send the gateway key as a bearer token to `/generate` and `/metrics`. Upstream keys are optional and use `EMBEDDING_API_KEY` and `GENERATION_API_KEY`. Use `CACHE_TTL_SECONDS`, `CACHE_CAPACITY`, `CACHE_SIMILARITY` and `LISTEN` to override the defaults.

Each process serves one tenant and one generation policy. Use separate instances for different authorization scopes. Send `"cacheable":false` when a request needs a fresh answer. This bypasses embedding and caching.

The cache stores each generated answer under its original prompt. Exact matches skip the embedding call and return `similarity: null` with `cache_hit: true`. Send `"exact_only":true` to cache repeated prompt text while skipping embedding and cosine lookup on misses. Exact-only entries are eligible for their original prompt, and a semantic hit never creates an exact entry for the matching query.

Concurrent misses for the same prompt and cache mode share an in-flight request. Exact-only requests have their own group so they cannot inherit a semantic candidate. All admitted callers receive the same result, and work continues if the originating client disconnects. A response with `coalesced: true` joined an active request.

Set `CACHE_MAX_INFLIGHT` to cap active groups and `CACHE_MAX_WAITERS_PER_KEY` to cap followers per group. Both default to 64. New work beyond either limit receives HTTP 503 with `Retry-After: 1`. Check `semantic_inflight_keys`, `semantic_coalesced_requests_total`, and `semantic_overload_rejected_total` to distinguish active work, joined requests, and overload. Embedding, generation, hit, and miss counters count the actual work performed.

Cosine similarity can match prompts that require different answers. Measure the false-hit rate on representative prompt pairs. Include embedding costs on semantic lookups when estimating savings.
