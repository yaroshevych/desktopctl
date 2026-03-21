# Eval Workspace

This directory hosts dataset packaging, Label Studio curation inputs, and run outputs used for CV/tokenize regression evaluation.

## Dataset Unit

Each labeling unit lives in its own folder:

`eval/datasets/<dataset_path>/<label_id>/`

Required files:
- `image.png` (or `.jpg`)
- `overlay.png` (visualization to review)
- `label.json` (normalized candidate labels)
- `metadata.json`

`metadata.json` must include at least:
- `run_id`
- `label_id`
- `sample_id`
- `dataset`
- `control_type` (for example `text_field`, `button`, `list`)
- `detector_commit`

## Identity Rules

- `sample_id`: identity of the underlying screenshot/source sample.
- `label_id`: identity of a specific labeling task.

The same `sample_id` can appear multiple times with different `label_id` values when curating different control types.

## Label Studio Flow

1. Generate candidates and overlays into `eval/datasets/.../<label_id>/`.
2. Import tasks into Label Studio with links to `image`, `overlay`, and metadata.
3. Curate in Label Studio (accept/correct/reject/ignore).
4. Export labeled results for test set generation.

Label sets are maintained in Label Studio (Postgres-backed).

## Test Run Workflow

```bash
# 1. Export accepted labels from LS into a run folder
just run-export PROJECT_ID=3 SLUG=text_fields CONTROL=text_fields
# → datasets/runs/<timestamp>-text_fields/artifacts.csv

# 2. Run tokenizer on images listed in artifacts.csv
#    (writes overlay.png + label.json + metadata.json per label_id into results/)

# 3. Import tokenizer results into LS (project name = run_id)
just run-import 20260321-143000-text_fields

# 4. Label in LS (accept / reject / follow_up)

# 5. Log metrics to MLflow
just run-log 20260321-143000-text_fields
```

Run folder layout:

```
datasets/runs/<timestamp>-<slug>/
  artifacts.csv       ← source images exported from LS accepted labels
  results/
    <label_id>/
      overlay.png     ← tokenizer output rendered
      label.json      ← tokenizer output as JSON
      metadata.json   ← run_id, source label_id, git hash, params
```

## Label Studio Tips

### Auto-submit on keypress (1/2/3)

LS selects choices with 1/2/3 but doesn't auto-submit. Paste this in devtools console
once per page load to auto-submit when a key is pressed:

```js
document.addEventListener("keydown", function(e) {
  if (!["1","2","3"].includes(e.key)) return;
  setTimeout(function() {
    Array.from(document.querySelectorAll("button"))
      .find(b => b.textContent.trim() === "Submit")
      ?.click();
  }, 300);
});
```

## MLflow Contract

Every test run is tracked by `run_id` in MLflow.

Recommended artifact layout per run:

`eval/datasets/runs/<run_id>/<label_id>/...`

Store exported labels under the run, including snapshots of labels marked `gold` in Label Studio.

## Gold Labels

Gold samples stay in `eval/datasets/...` (no separate `eval/golden/...` tree).

Gold membership is tracked in the Label Studio database by marking samples as `gold`.

For regression runs, export the currently `gold`-marked labels and use that export as the test input snapshot.

## Gold Snapshot Format (For Tests)

Tests must not read Label Studio DB directly. They should read machine-readable exports saved under a run folder.

Recommended files:
- `eval/datasets/runs/<run_id>/gold/gold_sets.json`
- `eval/datasets/runs/<run_id>/gold/gold_artifacts.csv`

`gold_sets.json` should contain the selected gold label IDs grouped by control type and dataset.

`gold_artifacts.csv` should contain one row per selected sample artifact, for example:
- `label_id`
- `sample_id`
- `control_type`
- `dataset`
- `image_path`
- `overlay_path`
- `label_path`
- `metadata_path`

Rust regression tests should consume these snapshot files only (or generated test fixtures derived from them).

## Current First Target

- Control type: `text_field`
- Sources: MacPaw external set + own dataset
- Then repeat the same loop for `button`, `list`, and other controls.
