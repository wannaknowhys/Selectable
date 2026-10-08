# models/ — NEVER committed (see .gitignore)

This directory holds runtime artifacts only:

- `<tier>/det.onnx`, `<tier>/rec.onnx`, `<tier>/inference.yml` (char dict is parsed
  from the yml at startup, so it always matches the weights)
- `translate/enzh/`, `translate/zhen/` (phase 2, Bergamot)

Fetch them with:

```cmd
node tools\fetch-models.mjs --tier small
node tools\fetch-models.mjs --tier medium
node tools\fetch-models.mjs --all
```

Pinned commits live in `tools/models.lock.json`.
