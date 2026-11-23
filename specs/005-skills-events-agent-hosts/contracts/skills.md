# Contract: Skills, Resources and the prompt

Target: extension `io.modelcontextprotocol/skills` (Beta) on protocol revision `2026-07-28`, checked against `modelcontextprotocol/ext-skills` at `1631b3f1` (#368).

## Capability (in `server/discover`)

```json
{ "capabilities": { "tools": {}, "resources": {}, "prompts": {},
    "extensions": { "io.modelcontextprotocol/skills": {}, "io.modelcontextprotocol/events": {} } } }
```

`directoryRead` is not declared; `resources/directory/read` answers `-32601`.

## `skills/list`

Request params: optional `cursor`, plus `_meta`. One page; no `nextCursor`.

```json
{ "resultType": "complete",
  "skills": [ {
    "uri": "skill://musaic-work-delegation/SKILL.md",
    "frontmatter": { "name": "musaic-work-delegation", "description": "…", "metadata": { "version": "1.0.0" } },
    "resources": [
      { "uri": "skill://musaic-work-delegation/SKILL.md", "digest": "sha256:<64 hex>", "size": 0 },
      { "uri": "skill://musaic-work-delegation/references/work-lifecycle.md", "digest": "sha256:<64 hex>", "size": 0 }
    ] } ],
  "ttlMs": 3600000, "cacheScope": "public" }
```

- `frontmatter` equals the parsed frontmatter of the served `SKILL.md`, every key passed through.
- `resources` lists every served file exactly once, including `SKILL.md`.

## `skills/get`

Params `{ "uri": "skill://musaic-work-delegation/SKILL.md" }`. Result `{ "resultType": "complete", "skill": <Skill>, "ttlMs": 3600000, "cacheScope": "public" }`. Any other URI: `-32602`. Works whether or not the skill is listed.

## `resources/list`, `resources/read` (both profiles)

`resources/list` returns each served file: `uri`, `name` (the file name; for `SKILL.md` the frontmatter `name`), `mimeType: "text/markdown"`, and for `SKILL.md` the frontmatter `description`. `resources/read` returns `contents: [{uri, mimeType, text}]` with exactly the embedded bytes.

## Refusals

A URI is served only if it equals a manifest URI byte for byte. Refused with `-32602` and the fixed message `unknown resource`: `..` segments, `%`-encoded input, `\`, control characters, query or fragment, trailing slash, absolute file paths, `file:`/`http(s):`/any other scheme, a different first segment (another server's or another skill's identifier), and unlisted names. The message never echoes the request.

## Legacy sessions (`2025-11-25`)

`skills/list`, `skills/get`: `-32601`. Resources and prompts work.

## Prompt `musaic_research_album`

`prompts/list`: `{name: "musaic_research_album", description, arguments: [{name: "album_id", required: false}]}`. `prompts/get` returns one user message: the instruction line, then the `SKILL.md` body. `album_id`, when given, must be 16 lowercase hex characters, else `-32602`.

## Tests (conformance harness)

1. Modern client: list → one skill; for every manifest entry, read and compare SHA-256 and size.
2. `skills/get` by URI succeeds when the listing is configured empty.
3. Each refusal case returns `-32602`, identical message.
4. Legacy client: `-32601` for skill methods; resource and prompt return the same bytes as the modern read.
5. Frontmatter in the entry equals frontmatter parsed from the bytes read.
