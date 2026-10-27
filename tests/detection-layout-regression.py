"""Compare native detection on an actual recording in different video canvases.

Supply a short source-time window containing both normal play and a pause.
Derived fixtures keep the whole game image and preserve frame rate. They test
padding, scaling and offset, not independent native game UI layouts.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--start", type=float, required=True)
    parser.add_argument("--duration", type=float, default=5)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--media-dir", type=Path, required=True)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, help="Reuse previously generated fixtures")
    args = parser.parse_args()
    root = args.run_dir.resolve()
    root.mkdir(parents=True, exist_ok=False)
    fixture_dir = args.fixtures.resolve() if args.fixtures else root / "fixtures"
    fixture_dir.mkdir(parents=True, exist_ok=True)
    variants = [
        ("16x9", "scale=960:540"),
        ("16x10", "scale=960:540,pad=960:600:(ow-iw)/2:(oh-ih)/2"),
        ("4x3", "scale=960:540,pad=960:720:(ow-iw)/2:(oh-ih)/2"),
        ("21x9", "scale=960:540,pad=1260:540:(ow-iw)/2:(oh-ih)/2"),
        ("1x1", "scale=960:540,pad=960:960:(ow-iw)/2:(oh-ih)/2"),
        ("9x16", "scale=576:324,pad=576:1024:(ow-iw)/2:(oh-ih)/2"),
        ("offset", "scale=960:540,pad=1280:800:80:40"),
    ]
    summary = {"status": "running", "cliSha256": hashlib.sha256(args.cli.read_bytes()).hexdigest(),
               "scope": "Derived full-content canvases, not independent native game layouts",
               "cases": [], "failures": []}
    baseline = None
    for name, vf in variants:
        movie = fixture_dir / (name + ".mp4")
        if not args.fixtures:
            subprocess.run([str(args.media_dir / "ffmpeg.exe"), "-hide_banner", "-v", "error", "-n",
                            "-ss", str(args.start), "-i", str(args.source), "-t", str(args.duration),
                            "-map", "0:v:0", "-an", "-vf", vf + ",setsar=1", "-fps_mode", "passthrough",
                            "-c:v", "libx264", "-preset", "fast", "-crf", "12", "-threads", "2", str(movie)],
                           check=True, timeout=180, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        folder = root / name
        folder.mkdir()
        (folder / "data").mkdir()
        (folder / "temp").mkdir()
        request = folder / "request.json"
        write(request, {"op": "import", "payload": {"path": str(movie)}})
        env = dict(os.environ, ARKCUT_MEDIA_DIR=str(args.media_dir.resolve()),
                   ARKCUT_DATA_DIR=str(folder / "data"), TEMP=str(folder / "temp"), TMP=str(folder / "temp"))
        for key in ("ARKCUT_RESOURCE_TEST_FILE", "ARKCUT_COMPONENT_TEST_FILE", "ARKCUT_UPDATE_TEST_FILE"):
            env.pop(key, None)
        started = time.monotonic()
        with (folder / "result.json").open("wb") as out, (folder / "stderr.txt").open("wb") as err:
            proc = subprocess.run([str(args.cli.resolve()), str(request)], env=env, stdout=out, stderr=err,
                                  timeout=180, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        result = read(folder / "result.json")
        job = result.get("job", {})
        project = job.get("result") or {}
        write(folder / "project.arkcut.json", project)
        pauses = project.get("pauses", [])
        bounds = [(p.get("transition") or {}).get("originalBounds", [p["startUs"], p["endUs"]]) for p in pauses]
        detection = project.get("detection", {})
        pts = project.get("source", {}).get("framePts", [])
        tolerance = max((b-a for a, b in zip(pts, pts[1:])), default=33334) + 2
        passed = proc.returncode == 0 and job.get("status") == "completed" and detection.get("status") == "completed"
        if baseline is None:
            baseline = bounds
            passed = passed and bool(bounds)
        else:
            passed = passed and len(bounds) == len(baseline) and all(
                abs(a-x) <= tolerance and abs(b-y) <= tolerance for (a, b), (x, y) in zip(bounds, baseline))
        row = dict(case=name, passed=passed, seconds=round(time.monotonic()-started, 3),
                   detection=detection, pauseBounds=bounds, toleranceUs=tolerance, error=job.get("error"),
                   transitionDecisions=[(p.get("transition") or {}).get("decision") for p in pauses])
        summary["cases"].append(row)
        if not passed:
            summary["failures"].append(name)
        write(root / "summary.json", summary)
        print(json.dumps({"case": name, "passed": passed, "status": detection.get("status"), "pauseBounds": bounds}), flush=True)
    summary["status"] = "passed" if not summary["failures"] else "failed"
    write(root / "summary.json", summary)
    return 0 if summary["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
