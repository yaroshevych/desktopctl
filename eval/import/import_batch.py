#!/usr/bin/env python3
"""Import a label dataset directory into Label Studio.

Scans a directory for label subdirs (each containing image.png, overlay.png,
label.json, metadata.json) and creates one LS task per subdir. Files stay on
disk — only task records are created in LS pointing to them via local-files URLs.

Usage:
    uv run eval/import/import_batch.py \\
        --dir eval/datasets/path/to/dataset/labels/text_fields \\
        --project "golden-set"

    # preview without importing
    uv run eval/import/import_batch.py --dir ... --project ... --dry-run

Environment (from eval/.env):
    LS_URL      Label Studio base URL (default: http://localhost:8080)
    LS_API_KEY  Legacy API token from LS UI → Account & Settings → Access Token
    DATA_DIR    Host path mounted as /data/local-files inside the LS container
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

import requests
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")

LS_LABEL_CONFIG = """
<View>
  <Image name="overlay" value="$overlay_image" zoom="true"/>
  <Choices name="verdict" toName="overlay" choice="single" required="true"
           showInLine="true">
    <Choice value="gold"/>
    <Choice value="accept"/>
    <Choice value="reject"/>
    <Choice value="ignore"/>
  </Choices>
  <TextArea name="comments" toName="overlay"
            placeholder="Comments (optional)" rows="2"
            perRegion="false"/>
</View>
"""

LS_RUN_LABEL_CONFIG = """
<View>
  <Image name="overlay" value="$overlay_image" zoom="true"/>
  <Choices name="verdict" toName="overlay" choice="single" required="true"
           showInLine="true">
    <Choice value="accept"/>
    <Choice value="reject"/>
    <Choice value="follow_up"/>
  </Choices>
  <TextArea name="comments" toName="overlay"
            placeholder="Comments (optional)" rows="2"
            perRegion="false"/>
</View>
"""


def ls_headers(api_key: str) -> dict[str, str]:
    return {"Authorization": f"Token {api_key}", "Content-Type": "application/json"}


def get_or_create_project(base_url: str, api_key: str, name: str, storage_path: str | None = None, label_config: str = LS_LABEL_CONFIG) -> int:
    hdrs = ls_headers(api_key)
    r = requests.get(f"{base_url}/api/projects", headers=hdrs)
    r.raise_for_status()
    for proj in r.json().get("results", []):
        if proj["title"] == name:
            print(f"  using existing project #{proj['id']}: {name}")
            return proj["id"]

    r = requests.post(
        f"{base_url}/api/projects",
        headers=hdrs,
        json={"title": name, "label_config": label_config},
    )
    r.raise_for_status()
    proj_id = r.json()["id"]
    print(f"  created project #{proj_id}: {name}")

    if storage_path:
        r2 = requests.post(
            f"{base_url}/api/storages/localfiles",
            headers=hdrs,
            json={"project": proj_id, "title": "files", "path": storage_path, "use_blob_urls": True},
        )
        r2.raise_for_status()
        print(f"  created storage: {storage_path}")

    return proj_id


def path_to_ls_url(path: Path, data_dir: Path) -> str:
    """Convert absolute host path → LS local-files URL."""
    rel = path.relative_to(data_dir)
    return f"/data/local-files/?d={rel}"


def collect_label_dirs(scan_dir: Path) -> list[Path]:
    """Find all label subdirs (dirs containing overlay.png + metadata.json)."""
    dirs = []
    for entry in sorted(scan_dir.iterdir()):
        if entry.is_dir() and (entry / "overlay.png").exists() and (entry / "metadata.json").exists():
            dirs.append(entry)
    return dirs


def build_task(label_dir: Path, data_dir: Path) -> dict:
    metadata = json.loads((label_dir / "metadata.json").read_text())
    overlay_path = label_dir / "overlay.png"
    image_path = label_dir / "image.png"
    # Overlay-only runs skip image copy for speed; use overlay as the original panel fallback.
    image_url_path = image_path if image_path.exists() else overlay_path

    data: dict = {
        "overlay_image": path_to_ls_url(overlay_path, data_dir),
        "image": path_to_ls_url(image_url_path, data_dir),
        "label_id": metadata.get("label_id", label_dir.name),
        "sample_id": metadata.get("sample_id", ""),
        "control_type": metadata.get("control_type", ""),
        "dataset": metadata.get("dataset", ""),
        "metadata": metadata,
    }
    return {"data": data}


def import_tasks(base_url: str, api_key: str, project_id: int, tasks: list[dict]) -> None:
    hdrs = ls_headers(api_key)
    # LS import endpoint accepts up to 250 tasks at a time
    batch_size = 250
    total = 0
    for i in range(0, len(tasks), batch_size):
        batch = tasks[i : i + batch_size]
        r = requests.post(
            f"{base_url}/api/projects/{project_id}/import",
            headers=hdrs,
            json=batch,
        )
        r.raise_for_status()
        total += r.json().get("task_count", len(batch))
    print(f"  imported: {total} tasks")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", required=True, help="Directory containing label subdirs")
    ap.add_argument("--project", required=True, help="LS project name (created if missing)")
    ap.add_argument("--ls-url", default=os.getenv("LS_URL", "http://localhost:8080"))
    ap.add_argument("--ls-key", default=os.getenv("LS_API_KEY"), help="LS API token")
    ap.add_argument("--data-dir", default=os.getenv("DATA_DIR"), help="Host path mounted as /data/local-files")
    ap.add_argument("--run", action="store_true", help="Use run label config (accept/reject/follow_up)")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if not args.ls_key:
        sys.exit("LS_API_KEY not set — add to eval/.env or pass --ls-key")
    if not args.data_dir:
        sys.exit("DATA_DIR not set — add to eval/.env or pass --data-dir")

    scan_dir = Path(args.dir).resolve()
    data_dir = Path(args.data_dir).resolve()

    if not scan_dir.is_dir():
        sys.exit(f"not a directory: {scan_dir}")
    if not scan_dir.is_relative_to(data_dir):
        sys.exit(f"--dir must be inside DATA_DIR\n  dir:      {scan_dir}\n  DATA_DIR: {data_dir}")

    print(f"scanning {scan_dir} ...")
    label_dirs = collect_label_dirs(scan_dir)
    print(f"  found {len(label_dirs)} label dirs")
    if not label_dirs:
        sys.exit("nothing to import")

    tasks = [build_task(d, data_dir) for d in label_dirs]

    if args.dry_run:
        print("dry-run — first task preview:")
        print(json.dumps(tasks[0], indent=2))
        return

    # storage path = /data/local-files/<top-level-segment> so LS can serve all files in the tree
    rel = scan_dir.relative_to(data_dir)
    storage_path = "/data/local-files/" + rel.parts[0]

    label_config = LS_RUN_LABEL_CONFIG if args.run else LS_LABEL_CONFIG
    project_id = get_or_create_project(args.ls_url, args.ls_key, args.project, storage_path=storage_path, label_config=label_config)
    import_tasks(args.ls_url, args.ls_key, project_id, tasks)
    print(f"done → {args.ls_url}/projects/{project_id}/")


if __name__ == "__main__":
    main()
