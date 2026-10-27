"""Real-source pause-transition regression against frozen visual annotations.

No source movie or existing project is changed. ``analyze`` clones the two
historical projects into a dedicated run directory and invokes the native core.
``verify`` compares EXTRA removals with independent, source-time annotations.
An abstained positive is reported, not treated as a safety failure. Unannotated
extra removals require separate visual review; they cannot silently pass.

Usage (run from the repository root):
  python tests/pause-transition-regression.py --phase analyze
  python tests/pause-transition-regression.py --phase verify
  python tests/pause-transition-regression.py --phase export

Choose a fresh --run-dir after rebuilding the CLI; --resume only reuses results
whose saved CLI and annotation hashes still match. No cleanup/delete is done.
The repository-relative defaults describe the local validation layout; private
movies, projects and annotations are not distributed. Supply --baseline-pc,
--baseline-mobile and --annotations to reuse the checks with another dataset.
"""
from __future__ import annotations

import argparse
import bisect
import collections
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_RUN = ROOT / "outputs/pause-transition-validation/run"
DEFAULT_ANNOTATIONS = ROOT / "outputs/pause-transition-validation/annotations/annotations.json"
BASELINES = {
    kind: ROOT / f"outputs/0.16.2/验收证据/{kind}/export-project.arkcut.json"
    for kind in ("pc", "mobile")
}


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def merge(intervals):
    out = []
    for a, b in sorted(intervals):
        if b <= a:
            continue
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def subtract(intervals, excluded):
    result = []
    for a, b in merge(intervals):
        cursor = a
        for x, y in merge(excluded):
            if y <= cursor:
                continue
            if x >= b:
                break
            if x > cursor:
                result.append([cursor, min(x, b)])
            cursor = max(cursor, y)
            if cursor >= b:
                break
        if cursor < b:
            result.append([cursor, b])
    return result


def intersect(intervals, a, b):
    return merge([max(a, x), min(b, y)] for x, y in intervals if x < b and y > a)


def duration(intervals):
    return sum(b - a for a, b in merge(intervals))


def frame_count(intervals, pts):
    return sum(bisect.bisect_left(pts, b) - bisect.bisect_left(pts, a) for a, b in merge(intervals))


def removed(project):
    return merge([p["startUs"], p["endUs"]] for p in project["pauses"]
                 if p["enabled"] and p["state"] == "confirmed")


def check_interval_helpers():
    # Verify safety accounting independently of the production timeline code.
    assert merge([[2, 4], [0, 2], [6, 7], [6, 9]]) == [[0, 4], [6, 9]]
    assert subtract([[0, 10], [12, 16]], [[2, 3], [5, 13]]) == [[0, 2], [3, 5], [13, 16]]
    assert subtract([[2, 4]], [[0, 5]]) == []
    assert intersect([[0, 2], [4, 8]], 2, 5) == [[4, 5]]
    assert frame_count([[1, 8]], [0, 1, 3, 7, 8, 12]) == 3


def native_call(args, kind, stage, op, payload):
    folder = args.run_dir / kind
    folder.mkdir(parents=True, exist_ok=True)
    data, temp = folder / "data", folder / "temp"
    data.mkdir(exist_ok=True)
    temp.mkdir(exist_ok=True)
    request = folder / f"{stage}-request.json"
    stdout = folder / f"{stage}-result.json"
    stderr = folder / f"{stage}-stderr.txt"
    save(request, {"op": op, "payload": payload})
    env = {**os.environ, "ARKCUT_MEDIA_DIR": str(ROOT / "tools/media"),
           "ARKCUT_DATA_DIR": str(data), "TEMP": str(temp), "TMP": str(temp)}
    for key in ("ARKCUT_RESOURCE_TEST_FILE", "ARKCUT_UPDATE_TEST_FILE", "ARKCUT_COMPONENT_TEST_FILE"):
        env.pop(key, None)
    began = time.monotonic()
    proc = None
    try:
        with stdout.open("wb") as out, stderr.open("wb") as err:
            proc = subprocess.Popen([str(args.cli), str(request)], stdout=out, stderr=err, env=env,
                                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            save(folder / f"{stage}-process.json", {"pid": proc.pid, "cli": str(args.cli), "stage": stage})
            while True:
                try:
                    proc.wait(timeout=20)
                    break
                except subprocess.TimeoutExpired:
                    elapsed = time.monotonic() - began
                    print(f"{kind} {stage}: {elapsed:.0f}s elapsed", flush=True)
                    if elapsed > args.timeout:
                        raise TimeoutError(f"{kind} {stage} exceeded {args.timeout}s")
        state = read(stdout) if stdout.stat().st_size else {}
        job = state.get("job", {})
        row = {"operation": op, "seconds": round(time.monotonic() - began, 3),
               "exitCode": proc.returncode, "jobStatus": job.get("status"),
               "error": job.get("error"), "resultPath": str(stdout), "stderrPath": str(stderr)}
        save(folder / f"{stage}-execution.json", row)
        if proc.returncode != 0 or (job and job.get("status") != "completed"):
            message = job.get("error") or stderr.read_text(encoding="utf-8", errors="replace")[-5000:]
            raise RuntimeError(f"{kind} {stage} failed: {message}")
        value = job.get("result", state)
        if not isinstance(value, dict):
            raise RuntimeError(f"{kind} {stage}: missing structured result")
        print(f"{kind} {stage}: completed in {row['seconds']:.3f}s", flush=True)
        return value, row
    finally:
        if proc is not None and proc.poll() is None:
            # Only terminate this test's still-live child process tree.
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=20)
            else:
                proc.kill()
            proc.wait(timeout=20)


def run_analysis(args):
    if not args.cli.is_file():
        raise FileNotFoundError(f"CLI not built: {args.cli}")
    annotation_sha, cli_sha = sha(args.annotations), sha(args.cli)
    summary = {"schemaVersion": 1, "phase": "analyze", "status": "running",
               "cli": str(args.cli), "cliSha256": cli_sha, "annotationsSha256": annotation_sha,
               "runDirectory": str(args.run_dir), "videos": []}
    path = args.run_dir / "analysis-summary.json"
    save(path, summary)
    try:
        for kind in args.videos:
            folder = args.run_dir / kind
            metadata_path = folder / "run-metadata.json"
            output_path = folder / "analyzed.arkcut.json"
            if metadata_path.exists():
                metadata = read(metadata_path)
                if not args.resume:
                    raise RuntimeError(f"Existing {kind} run: use a fresh --run-dir or --resume")
                if metadata["cliSha256"] != cli_sha or metadata["annotationsSha256"] != annotation_sha:
                    raise RuntimeError(f"{kind}: CLI/annotation hash changed; choose a fresh --run-dir")
                if output_path.exists():
                    summary["videos"].append({"video": kind, "status": "reused", "project": str(output_path)})
                    save(path, summary)
                    continue
                clone = read(folder / "input.arkcut.json")
            else:
                baseline = args.baselines[kind]
                original = read(baseline)
                folder.mkdir(parents=True, exist_ok=True)
                (folder / "baseline.arkcut.json").write_bytes(baseline.read_bytes())
                clone = copy.deepcopy(original)
                clone["projectId"] = str(uuid.uuid4())
                clone["revision"] = 1
                clone["name"] = f"pause-transition-validation-{kind}"
                clone.pop("completedExport", None)
                clone.pop("outputPath", None)
                clone["music"] = []
                clone["mix"]["currentTrack"] = None
                metadata = {"video": kind, "cli": str(args.cli), "cliSha256": cli_sha,
                            "annotations": str(args.annotations), "annotationsSha256": annotation_sha,
                            "baselinePath": str(baseline), "baselineSha256": sha(baseline),
                            "sourceFingerprint": original["source"]["fingerprint"],
                            "originalProjectId": original["projectId"], "isolatedProjectId": clone["projectId"]}
                save(metadata_path, metadata)
                save(folder / "input.arkcut.json", clone)
            result, execution = native_call(args, kind, "analyze", "analyze", {"project": clone})
            if result.get("status") != "completed":
                raise RuntimeError(f"{kind}: detection status is {result.get('status')!r}")
            clone["pauses"] = result["pauses"]
            for key in ("status", "geometry", "region"):
                clone["detection"][key] = result[key]
            clone["detection"]["profile"] = result["version"]
            versions = sorted({p["transition"]["version"] for p in clone["pauses"] if p.get("transition")})
            if len(versions) != 1:
                raise RuntimeError(f"{kind}: no consistent transition review version; was the new CLI built?")
            clone["detection"]["version"] = versions[0]
            clone["revision"] += 1
            save(output_path, clone)
            unchanged = sha(args.baselines[kind]) == metadata["baselineSha256"]
            if not unchanged:
                raise AssertionError(f"{kind}: original baseline file changed during analysis")
            decisions = collections.Counter(p.get("transition", {}).get("decision", "missing") for p in clone["pauses"])
            summary["videos"].append({"video": kind, "status": "completed", "project": str(output_path),
                                      "pauseCount": len(clone["pauses"]), "transitionVersion": versions[0],
                                      "decisions": dict(decisions), "execution": execution,
                                      "originalBaselineUnchanged": unchanged})
            save(path, summary)
        summary["status"] = "completed"
    except BaseException as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
        raise
    finally:
        save(path, summary)


def verify(args):
    frozen = read(args.annotations)
    annotation_sha = sha(args.annotations)
    cases = frozen["cases"]
    supplements = []
    if args.supplement:
        supplemental = read(args.supplement)
        supplements = supplemental["cases"] if isinstance(supplemental, dict) else supplemental
    summary = {"schemaVersion": 1, "phase": "verify", "status": "running",
               "annotations": str(args.annotations), "annotationsSha256": annotation_sha,
               "annotationLimits": frozen.get("verificationLimits", []),
               "supplement": str(args.supplement) if args.supplement else None,
               "supplementSha256": sha(args.supplement) if args.supplement else None,
               "videos": [], "hardFailures": []}
    protected, positives, unreviewed = [], [], []
    for kind in args.videos:
        folder = args.run_dir / kind
        before, after = read(folder / "baseline.arkcut.json"), read(folder / "analyzed.arkcut.json")
        metadata = read(folder / "run-metadata.json")
        pts = before["source"]["framePts"]
        bounds = set(pts) | {before["source"]["durationUs"]}
        old_by_id = {p["id"]: p for p in before["pauses"]}
        new_by_id = {p["id"]: p for p in after["pauses"]}
        invariants = []
        if annotation_sha != metadata["annotationsSha256"]:
            invariants.append("frozen annotation file changed after analyze")
        if sha(args.baselines[kind]) != metadata["baselineSha256"]:
            invariants.append("original baseline project changed")
        if sha(folder / "baseline.arkcut.json") != metadata["baselineSha256"]:
            invariants.append("saved baseline snapshot changed")
        if before["source"] != after["source"]:
            invariants.append("source metadata/PTS changed")
        if set(old_by_id) != set(new_by_id):
            invariants.append("original pause identity set changed")
        changed = []
        for id, current in new_by_id.items():
            original = old_by_id.get(id)
            if original is None:
                continue
            old = [original["startUs"], original["endUs"]]
            new = [current["startUs"], current["endUs"]]
            review = current.get("transition") or {}
            if review.get("originalBounds") != old:
                invariants.append(f"{id}: original UI pause bounds not preserved in transition metadata")
            if review.get("appliedBounds") != new or current.get("detectedBounds") != new:
                invariants.append(f"{id}: applied/detected boundary metadata inconsistent")
            if any(current[k] != original[k] for k in ("enabled", "state", "source")):
                invariants.append(f"{id}: pause enablement/state/source changed")
            if new[1] != old[1] or not old[0] - 80000 <= new[0] <= old[0]:
                invariants.append(f"{id}: unexpected pause shrink/end change or rollback beyond80ms")
            if new[0] not in bounds or new[1] not in bounds:
                invariants.append(f"{id}: boundary is not an actual frame PTS")
            if old != new:
                changed.append({"pauseId": id, "originalBounds": old, "appliedBounds": new,
                                "decision": review.get("decision"), "evidence": review.get("evidence")})
        old_removed, new_removed = removed(before), removed(after)
        extra = subtract(new_removed, old_removed)
        lost = subtract(old_removed, new_removed)
        if lost:
            invariants.append("some originally removed pause content is no longer removed")
        video_cases = []
        for case in [c for c in cases + supplements if c["video"] == kind]:
            a, b = case["startUs"], case["endUs"]
            overlap = intersect(extra, a, b)
            row = {"id": case["id"], "video": kind, "label": case["label"],
                   "startUs": a, "endUs": b, "sourceFrames": frame_count([[a, b]], pts),
                   "extraRemovedFrames": frame_count(overlap, pts), "extraRemovedUs": duration(overlap),
                   "extraOverlap": overlap}
            if case["label"].startswith("preserve"):
                row["passed"] = not overlap
                row["status"] = "preserved" if row["passed"] else "FALSE_DELETION"
                protected.append(row)
                if overlap:
                    summary["hardFailures"].append(f"{case['id']}: protected source frames additionally deleted")
            elif case["label"] == "removePauseAttachedTransition":
                row["status"] = ("fullyRemoved" if duration(overlap) == b - a
                                 else "partiallyRemoved" if overlap else "abstained")
                positives.append(row)
            video_cases.append(row)
        positive_ranges = [[c["startUs"], c["endUs"]] for c in cases + supplements
                           if c["video"] == kind and c["label"] == "removePauseAttachedTransition"]
        unknown = subtract(extra, positive_ranges)
        if unknown:
            unreviewed.append({"video": kind, "ranges": unknown, "frames": frame_count(unknown, pts),
                               "durationUs": duration(unknown)})
        summary["hardFailures"].extend(f"{kind}: {x}" for x in invariants)
        summary["videos"].append({"video": kind, "pauseCountBefore": len(before["pauses"]),
                                  "pauseCountAfter": len(after["pauses"]), "changedPauses": changed,
                                  "extraRemoved": extra, "extraRemovedFrames": frame_count(extra, pts),
                                  "extraRemovedUs": duration(extra), "unreviewedExtra": unknown,
                                  "originalPauseBoundariesPreserved": not invariants,
                                  "invariantFailures": invariants, "cases": video_cases})
    summary["protected"] = {"cases": len(protected), "passed": sum(x["passed"] for x in protected),
                            "frames": sum(x["sourceFrames"] for x in protected),
                            "extraRemovedFrames": sum(x["extraRemovedFrames"] for x in protected),
                            "extraRemovedUs": sum(x["extraRemovedUs"] for x in protected),
                            "byLabel": dict(collections.Counter(x["label"] for x in protected))}
    summary["positives"] = {"cases": len(positives), "fullyRemoved": sum(x["status"] == "fullyRemoved" for x in positives),
                            "partiallyRemoved": sum(x["status"] == "partiallyRemoved" for x in positives),
                            "abstained": sum(x["status"] == "abstained" for x in positives),
                            "sourceFrames": sum(x["sourceFrames"] for x in positives),
                            "removedFrames": sum(x["extraRemovedFrames"] for x in positives)}
    summary["unreviewedExtra"] = unreviewed
    summary["status"] = "failed" if summary["hardFailures"] else "needs-review" if unreviewed else "passed"
    save(args.run_dir / "verification-summary.json", summary)
    print(json.dumps({k: summary[k] for k in ("status", "protected", "positives", "hardFailures", "unreviewedExtra")},
                     ensure_ascii=False, indent=2), flush=True)
    return 1 if summary["hardFailures"] else 2 if unreviewed else 0


def run_exports(args):
    summary = {"schemaVersion": 1, "phase": "export", "status": "running", "videos": []}
    path = args.run_dir / "export-summary.json"
    save(path, summary)
    try:
        for kind in args.videos:
            folder = args.run_dir / kind
            project = read(folder / "analyzed.arkcut.json")
            target = folder / "processed.mp4"
            if target.exists():
                raise FileExistsError(f"Existing export preserved: {target}")
            project["outputPath"] = str(target)
            result, execution = native_call(args, kind, "export", "export",
                                            {"project": project, "path": str(target), "formatChangeAccepted": True})
            summary["videos"].append({"video": kind, "path": str(target), "bytes": target.stat().st_size,
                                      "execution": execution, "audit": result.get("audit"),
                                      "plan": result.get("plan"), "effectiveProject": result.get("effectiveProject")})
            save(path, summary)
        summary["status"] = "completed"
    except BaseException as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
        raise
    finally:
        save(path, summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--phase", choices=("analyze", "verify", "export"), required=True)
    parser.add_argument("--cli", type=Path, default=ROOT / "src-tauri/target/release/arkcut-cli.exe")
    parser.add_argument("--run-dir", type=Path, default=DEFAULT_RUN)
    parser.add_argument("--annotations", type=Path, default=DEFAULT_ANNOTATIONS)
    parser.add_argument("--baseline-pc", type=Path, default=BASELINES["pc"], help="Original PC baseline project; read only")
    parser.add_argument("--baseline-mobile", type=Path, default=BASELINES["mobile"], help="Original mobile baseline project; read only")
    parser.add_argument("--supplement", type=Path, help="Separate visually reviewed additional cases; frozen file remains unchanged")
    parser.add_argument("--videos", nargs="+", choices=("pc", "mobile"), default=["pc", "mobile"])
    parser.add_argument("--resume", action="store_true", help="Only reuse same-CLI, same-annotations analysis results")
    parser.add_argument("--timeout", type=float, default=1800)
    args = parser.parse_args()
    args.cli, args.run_dir, args.annotations = args.cli.resolve(), args.run_dir.resolve(), args.annotations.resolve()
    args.baselines = {"pc": args.baseline_pc.resolve(), "mobile": args.baseline_mobile.resolve()}
    needed = {"--annotations": args.annotations} if args.phase in ("analyze", "verify") else {}
    if args.phase == "analyze":
        needed["--cli"] = args.cli
        needed.update({"--baseline-" + kind: args.baselines[kind] for kind in args.videos})
    missing = [flag for flag, location in needed.items() if not location.is_file()]
    if missing:
        parser.error("Missing inputs: " + ", ".join(missing) + ". Private recordings, baseline projects and annotations are not included in source delivery; supply your own explicit input paths.")
    args.run_dir.mkdir(parents=True, exist_ok=True)
    check_interval_helpers()
    if args.phase == "analyze":
        run_analysis(args)
        return 0
    if args.phase == "verify":
        return verify(args)
    run_exports(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
