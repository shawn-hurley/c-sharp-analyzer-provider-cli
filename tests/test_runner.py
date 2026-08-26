# /// script
# requires-python = ">=3.11"
# dependencies = ["pydantic>=2", "typer>=0.12"]
# ///
"""Test runner for the C# analyzer provider.

Subcommands:
    setup  — Clone external test repos defined by manifests
    run    — Run gRPC tests against a provider
    diff   — Compare two result directories

Usage:
    uv run tests/test_runner.py setup
    uv run tests/test_runner.py run --provider csharp
    uv run tests/test_runner.py diff results/csharp/latest results/csharp/previous
"""

import json
import shlex
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Annotated, Any, Generator, Optional

import typer
from pydantic import BaseModel

TESTDATA = Path(__file__).resolve().parent
REPOS = TESTDATA / "repos"
SUITES = TESTDATA / "suites"
RESULTS = TESTDATA / "results"

app = typer.Typer(help="Test runner for the C# analyzer provider.")


# ── Models ──────────────────────────────────────────────────────────────


class RepoConfig(BaseModel):
    url: str | None = None
    commit: str | None = None
    path: str | None = None


class Manifest(BaseModel):
    repo: RepoConfig = RepoConfig()
    location: str = ""
    steps: list[str] = []


class Provider(str, Enum):
    csharp = "csharp"


# ── Shared helpers ──────────────────────────────────────────────────────


def load_manifests(
    projects: list[str] | None = None,
) -> dict[str, Manifest]:
    manifests: dict[str, Manifest] = {}
    for manifest_path in sorted(SUITES.glob("*/_.json")):
        project = manifest_path.parent.name
        if projects and project not in projects:
            continue

        with open(manifest_path) as f:
            manifests[project] = Manifest.model_validate(json.load(f))

    return manifests


def resolve_repo_dir(project: str) -> Path:
    return REPOS / project


# ── setup ───────────────────────────────────────────────────────────────


STAMP_FILE = ".clone-commit"


def clone_repo(project: str, repo: RepoConfig) -> bool:
    dest = REPOS / project

    if not repo.url:
        if dest.exists():
            print(f"  {project}: in-tree, OK")
        else:
            print(f"  {project}: in-tree (no url), but directory missing!")
            return False
        return True

    if dest.exists():
        stamp = dest / STAMP_FILE
        if repo.commit and stamp.exists():
            actual = stamp.read_text().strip()
            if actual == repo.commit:
                print(f"  {project}: already exists @ {repo.commit[:12]}, skipping")
                return True
            else:
                print(f"  {project}: commit mismatch (have {actual[:12]}, want {repo.commit[:12]}), re-cloning")
                shutil.rmtree(dest)
        elif repo.commit and not stamp.exists():
            print(f"  {project}: no stamp file, re-cloning")
            shutil.rmtree(dest)
        else:
            print(f"  {project}: already exists, skipping")
            return True

    url = repo.url
    commit = repo.commit
    subpath = repo.path

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = Path(tmpdir) / "clone"

        print(f"  {project}: cloning {url} @ {commit[:12] if commit else 'HEAD'}")

        if commit:
            tmp.mkdir()
            git_steps = [
                ["git", "init"],
                ["git", "remote", "add", "origin", url],
                ["git", "fetch", "--depth", "1", "origin", commit],
                ["git", "checkout", "FETCH_HEAD"],
            ]
            for cmd in git_steps:
                result = subprocess.run(
                    cmd, capture_output=True, text=True, cwd=tmp
                )
                if result.returncode != 0:
                    print(f"    ERROR: {' '.join(cmd[:3])}... failed:\n{result.stderr.strip()}")
                    return False
        else:
            cmd = ["git", "clone", "--depth", "1", url, str(tmp)]
            result = subprocess.run(cmd, capture_output=True, text=True)
            if result.returncode != 0:
                print(f"    ERROR: git clone failed:\n{result.stderr.strip()}")
                return False

        if subpath:
            src = tmp / subpath
            if not src.is_dir():
                print(f"    ERROR: subdirectory {subpath} not found in clone")
                return False
            shutil.copytree(src, dest)
        else:
            shutil.copytree(tmp, dest, ignore=shutil.ignore_patterns(".git"))

    if commit:
        (dest / STAMP_FILE).write_text(commit + "\n")

    cs_files = list(dest.rglob("*.cs"))
    if not cs_files:
        print(f"    WARNING: no .cs files found in {dest}")

    print(f"    OK ({len(cs_files)} .cs files)")
    return True


@app.command()
def setup() -> None:
    """Clone external test repos defined by test manifests."""
    if not SUITES.is_dir():
        print(f"ERROR: {SUITES} does not exist")
        raise typer.Exit(1)

    manifests = load_manifests()
    if not manifests:
        print("No manifests found")
        raise typer.Exit(1)

    print(f"Found {len(manifests)} project(s)")
    REPOS.mkdir(parents=True, exist_ok=True)

    errors = []
    for project, manifest in manifests.items():
        if not clone_repo(project, manifest.repo):
            errors.append(project)

    if errors:
        print(f"\nFailed: {', '.join(errors)}")
        raise typer.Exit(1)
    else:
        print("\nAll repos ready")


# ── run ─────────────────────────────────────────────────────────────────


def grpcurl(port: int, data: dict[str, Any], method: str, max_time: int = 300) -> dict[str, Any] | None:
    cmd = [
        "grpcurl",
        "-max-msg-sz", "10485760",
        "-max-time", str(max_time),
        "-plaintext",
        "-d", json.dumps(data),
        f"127.0.0.1:{port}",
        f"provider.ProviderService/{method}",
    ]

    try:
        result = subprocess.run(
            cmd, capture_output=True, text=True, timeout=max_time + 30
        )
        if result.returncode != 0:
            return {"_error": result.stderr.strip()[:500]}
        return json.loads(result.stdout)  # type: ignore[no-any-return]
    
    except subprocess.TimeoutExpired:
        return {"_error": f"grpcurl timed out after {max_time}s"}
    except json.JSONDecodeError as e:
        return {"_error": f"invalid JSON response: {e}"}


def build_evaluate_request(data: dict[str, Any]) -> dict[str, Any]:
    request = dict(data)
    condition_info = request.get("conditionInfo")
    if isinstance(condition_info, dict):
        request["conditionInfo"] = json.dumps(condition_info)
    if "id" not in request:
        request["id"] = "1"
    return request


def normalize_response(response: dict[str, Any] | None, repo_dir: str) -> dict[str, Any]:
    if not response:
        return {}
    if "_error" in response:
        return response

    resp = response.get("response", {})
    incidents = resp.get("incidentContexts", [])

    repo_path = str(Path(repo_dir).resolve())

    for inc in incidents:
        uri = inc.get("fileURI", "")
        if uri.startswith("file:///"):
            file_path = uri[7:]  # keep leading / on unix
            if sys.platform == "win32":
                file_path = file_path.replace("/", "\\")
        elif uri.startswith("file://"):
            file_path = uri[7:]
            if sys.platform == "win32":
                file_path = file_path.replace("/", "\\")
        else:
            continue
        try:
            rel = Path(file_path).relative_to(repo_path)
            inc["fileURI"] = str(rel).replace("\\", "/")
        except ValueError:
            pass

    incidents.sort(
        key=lambda i: (
            i.get("fileURI", ""),
            i.get("codeLocation", {})
            .get("startPosition", {})
            .get("line", 0),
            i.get("codeLocation", {})
            .get("startPosition", {})
            .get("character", 0),
        )
    )

    return {
        "matched": resp.get("matched", False),
        "incidentCount": len(incidents),
        "incidents": incidents,
    }


def compare_results(actual: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if "_error" in actual:
        return False, f"actual has error: {actual['_error']}"

    actual_incidents = actual.get("incidents", [])
    expected_incidents = expected.get("incidents", [])

    if len(actual_incidents) != len(expected_incidents):
        return False, (
            f"incident count mismatch: "
            f"got {len(actual_incidents)}, expected {len(expected_incidents)}"
        )

    for i, (a, e) in enumerate(zip(actual_incidents, expected_incidents)):
        if a.get("fileURI") != e.get("fileURI"):
            return False, (
                f"incident {i}: fileURI mismatch: "
                f"{a.get('fileURI')} != {e.get('fileURI')}"
            )
        a_loc = a.get("codeLocation", {})
        e_loc = e.get("codeLocation", {})
        if a_loc != e_loc:
            return False, f"incident {i}: codeLocation mismatch"

    return True, "ok"


def normalize_init_response(response: dict[str, Any] | None) -> dict[str, Any]:
    """Reduce an Init response to the stable fields worth asserting.

    Drops machine-specific fields (builtinConfig.location, id) so goldens are
    portable across machines.
    """
    if not response or "_error" in response:
        return response or {}
    return {
        "successful": response.get("successful", False),
        "analysisMode": response.get("builtinConfig", {}).get("analysisMode"),
    }


def compare_init(actual: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if "_error" in actual:
        return False, f"actual has error: {actual['_error']}"

    a = normalize_init_response(actual)
    e = normalize_init_response(expected)

    if a.get("successful") != e.get("successful"):
        return False, (
            f"successful mismatch: got {a.get('successful')}, "
            f"expected {e.get('successful')}"
        )
    if a.get("analysisMode") != e.get("analysisMode"):
        return False, (
            f"analysisMode mismatch: got {a.get('analysisMode')}, "
            f"expected {e.get('analysisMode')}"
        )
    return True, "ok"


def validate_manifests(
    manifests: dict[str, Manifest], update: bool
) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []

    for project, manifest in manifests.items():
        test_dir = SUITES / project
        steps = manifest.steps

        for step_file in steps:
            step_path = test_dir / step_file
            if not step_path.exists():
                errors.append(f"{project}: step file {step_file} not found")

            expected = step_path.with_suffix("").with_suffix(".expected.json")
            if not update and not expected.exists():
                errors.append(
                    f"{project}: expected file {expected.name} not found "
                    f"(run with --update to generate)"
                )

        all_json = {
            f.name
            for f in test_dir.glob("*.json")
            if f.name != "_.json" and not f.name.endswith(".expected.json")
        }
        listed = set(steps)
        provider_overrides = {
            f"{Path(s).stem}.{p.value}.json"
            for s in steps
            for p in Provider
        }
        orphaned = all_json - listed - provider_overrides
        if orphaned:
            warnings.append(
                f"{project}: orphaned files not in steps: {', '.join(sorted(orphaned))}"
            )

        repo_dir = resolve_repo_dir(project)
        if not repo_dir.is_dir():
            warnings.append(
                f"{project}: repo dir {repo_dir} does not exist, "
                f"skipping (run setup first)"
            )

    return errors, warnings


def resolve_step_path(test_dir: Path, step_file: str, provider: Provider) -> Path:
    """Return provider-specific override if it exists, else the default."""
    stem = Path(step_file).stem
    override = test_dir / f"{stem}.{provider.value}.json"
    if override.exists():
        return override
    return test_dir / step_file


def inject_provider_config(send_data: dict[str, Any], provider: Provider) -> None:
    """Auto-discover tool paths for providers that need them."""
    pass


def resolve_init_location(
    project: str, manifest: Manifest, init_data: dict[str, Any],
    repo_root: Path | None = None,
) -> str:
    if repo_root:
        repo_dir = repo_root / project
    else:
        repo_dir = resolve_repo_dir(project)
    location = init_data.get("location", "")
    if location:
        return str(repo_dir / location)
    return str(repo_dir)


def wait_for_port(port: int, timeout: float = 60) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.5)
    return False


def _kill_tree(pid: int) -> None:
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(pid)],
            capture_output=True,
        )
    else:
        import os as _os
        _os.killpg(pid, signal.SIGKILL)


def _terminate_tree(pid: int) -> None:
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(pid)],
            capture_output=True,
        )
    else:
        import os as _os
        _os.killpg(pid, signal.SIGTERM)


@contextmanager
def managed_provider(cmd: str, port: int) -> Generator[subprocess.Popen[str], None, None]:
    kwargs: dict[str, Any] = dict(
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if sys.platform == "win32":
        kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        kwargs["start_new_session"] = True

    proc = subprocess.Popen(shlex.split(cmd), **kwargs)
    try:
        if not wait_for_port(port):
            _kill_tree(proc.pid)
            proc.wait()
            stderr = proc.stderr.read() if proc.stderr else ""
            raise RuntimeError(
                f"Provider did not start on port {port} within 60s\n{stderr[:500]}"
            )
        yield proc
    finally:
        _terminate_tree(proc.pid)
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            _kill_tree(proc.pid)
            proc.wait()


def run_project(
    project: str,
    manifest: Manifest,
    provider: Provider,
    port: int,
    result_dir: Path,
    *,
    update: bool,
    no_check: bool,
    verbose: bool,
    pause: bool,
    repo_root: Path | None = None,
) -> list[dict[str, str]]:
    test_dir = SUITES / project
    steps = manifest.steps
    repo_dir = resolve_repo_dir(project)
    results: list[dict[str, str]] = []

    if not repo_dir.is_dir():
        print(f"  SKIP: repo dir {repo_dir} does not exist")
        return results

    project_result_dir = result_dir / project
    project_result_dir.mkdir(parents=True, exist_ok=True)

    for step_file in steps:
        step_name = Path(step_file).stem
        step_path = resolve_step_path(test_dir, step_file, provider)

        with open(step_path) as f:
            request_data: dict[str, Any] = json.load(f)

        is_init = step_name == "init" or step_name.startswith("init-")

        if is_init:
            location = resolve_init_location(project, manifest, request_data, repo_root=repo_root)
            send_data = dict(request_data)
            send_data["location"] = location
            inject_provider_config(send_data, provider)
            print(f"  Init: {location}")
            if pause:
                input("    [--pause] Press Enter to send Init...")
            response = grpcurl(port, send_data, "Init", max_time=600)

            result_file = project_result_dir / f"{step_name}.result.json"
            result_file.write_text(json.dumps(response, indent=2))

            expected_path = test_dir / f"{step_name}.expected.json"

            if update:
                expected_path.write_text(
                    json.dumps(normalize_init_response(response), indent=2)
                )

            if no_check:
                status = "RAN"
            elif response and "_error" not in response:
                with open(expected_path) as f:
                    expected = json.load(f)
                ok, msg = compare_init(response, expected)
                status = "PASS" if ok else f"FAIL: {msg}"
            else:
                err_msg = (response or {}).get("_error", "unknown error")
                status = f"ERROR ({err_msg[:100]})"

            print(f"    {status}")
            results.append({
                "project": project,
                "step": step_name,
                "status": status,
            })

            if pause:
                input("    [--pause] Press Enter to continue...")
        else:
            send_data = build_evaluate_request(request_data)
            pattern = json.loads(send_data["conditionInfo"]).get(
                "referenced", {}
            ).get("pattern", "?")
            print(f"  Evaluate: {step_name} ({pattern})")
            if pause:
                input("    [--pause] Press Enter to send Evaluate...")
            response = grpcurl(port, send_data, "Evaluate")

            effective_repo = (repo_root / project) if repo_root else repo_dir
            repo_dir_str = str(effective_repo)
            location_field = manifest.location
            if location_field:
                repo_dir_str = str(effective_repo / location_field)

            normalized = normalize_response(response, repo_dir_str)

            result_file = project_result_dir / f"{step_name}.result.json"
            result_file.write_text(json.dumps(normalized, indent=2))

            expected_path = test_dir / f"{step_name}.expected.json"

            if update:
                expected_path.write_text(json.dumps(normalized, indent=2))
                count = normalized.get("incidentCount", 0)
                status = f"UPDATED ({count} incidents)"
                print(f"    {status}")
            elif no_check:
                count = normalized.get("incidentCount", 0)
                status = f"RAN ({count} incidents)"
                print(f"    {status}")
            elif "_error" in normalized:
                status = f"ERROR ({normalized['_error'][:100]})"
                print(f"    {status}")
            elif expected_path.exists():
                with open(expected_path) as f:
                    expected = json.load(f)
                ok, msg = compare_results(normalized, expected)
                if ok:
                    count = normalized.get("incidentCount", 0)
                    status = f"PASS ({count} incidents)"
                else:
                    status = f"FAIL: {msg}"
                print(f"    {status}")
                if not ok and verbose:
                    print(f"    Actual: {json.dumps(normalized, indent=2)[:500]}")
            else:
                status = "SKIP (no expected file)"
                print(f"    {status}")

            results.append({
                "project": project,
                "step": step_name,
                "status": status,
            })

            if pause:
                input("    [--pause] Press Enter to continue...")

    print("  Stop")
    grpcurl(port, {"id": "1"}, "Stop", max_time=10)

    return results


@app.command()
def run(
    provider: Annotated[Provider, typer.Option(help="Provider label for result directory")] = Provider.csharp,
    port: Annotated[int, typer.Option(help="Provider gRPC port")] = 9876,
    project: Annotated[Optional[list[str]], typer.Option(help="Run only named project(s)")] = None,
    update: Annotated[bool, typer.Option("--update", help="Overwrite golden files with actual results")] = False,
    no_check: Annotated[bool, typer.Option("--no-check", help="Skip golden file comparison")] = False,
    verbose: Annotated[bool, typer.Option("--verbose", help="Print full result JSON on failure")] = False,
    fail_fast: Annotated[bool, typer.Option("--fail-fast", help="Stop on first test failure")] = False,
    pause: Annotated[bool, typer.Option("--pause", help="Pause before and after each request (for debugging)")] = False,
    cmd: Annotated[Optional[str], typer.Option("--cmd", help="Command to start the provider (fresh per project)")] = None,
    repo_root: Annotated[Optional[str], typer.Option("--repo-root", help="Override repo path sent to the provider (for containers)")] = None,
) -> None:
    """Run gRPC tests against a provider."""
    if not shutil.which("grpcurl"):
        print("ERROR: grpcurl not found in PATH")
        print("Install: https://github.com/fullstorydev/grpcurl")
        raise typer.Exit(1)

    manifests = load_manifests(project)
    if not manifests:
        print("No test manifests found")
        raise typer.Exit(1)

    errors, warnings = validate_manifests(manifests, update)
    for w in warnings:
        print(f"WARNING: {w}")
    if errors:
        for e in errors:
            print(f"ERROR: {e}")
        raise typer.Exit(1)

    skip_projects = set()
    for proj, manifest in manifests.items():
        repo_dir = resolve_repo_dir(proj)
        if not repo_dir.is_dir():
            skip_projects.add(proj)

    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%S")
    result_dir = RESULTS / provider.value / timestamp
    result_dir.mkdir(parents=True, exist_ok=True)

    print(f"Provider: {provider.value} (port {port})")
    print(f"Results:  {result_dir}")
    print(f"Projects: {', '.join(manifests.keys())}")
    if skip_projects:
        print(f"Skipping: {', '.join(skip_projects)} (repo not cloned)")
    print()

    all_results: list[dict[str, str]] = []
    failed = False

    for proj, manifest in manifests.items():
        if proj in skip_projects:
            continue

        print(f"=== {proj} ===")

        _repo_root = Path(repo_root) if repo_root else None

        def _run_project() -> list[dict[str, str]]:
            return run_project(
                proj,
                manifest,
                provider,
                port,
                result_dir,
                update=update,
                no_check=no_check,
                verbose=verbose,
                pause=pause,
                repo_root=_repo_root,
            )

        if cmd:
            print(f"  Starting provider: {cmd}")
            try:
                with managed_provider(cmd, port):
                    results = _run_project()
            except RuntimeError as e:
                print(f"  ERROR: {e}")
                results = [{"project": proj, "step": "init", "status": f"ERROR ({e})"}]
        else:
            results = _run_project()

        all_results.extend(results)

        step_failed = any("FAIL" in r["status"] or "ERROR" in r["status"] for r in results)
        if step_failed:
            failed = True
        if step_failed and fail_fast:
            print("\n--fail-fast: stopping after first failure")
            break

        print()

    latest = RESULTS / provider.value / "latest"
    latest.unlink(missing_ok=True)
    latest.symlink_to(timestamp)

    print("=" * 50)
    print("Summary")
    print("=" * 50)
    for r in all_results:
        print(f"  {r['project']:25s} {r['step']:25s} {r['status']}")

    pass_count = sum(1 for r in all_results if "PASS" in r["status"])
    fail_count = sum(1 for r in all_results if "FAIL" in r["status"])
    error_count = sum(1 for r in all_results if "ERROR" in r["status"])
    update_count = sum(1 for r in all_results if "UPDATED" in r["status"])
    skip_count = sum(1 for r in all_results if "SKIP" in r["status"])

    print()
    parts = []
    if pass_count:
        parts.append(f"{pass_count} passed")
    if fail_count:
        parts.append(f"{fail_count} failed")
    if error_count:
        parts.append(f"{error_count} errors")
    if update_count:
        parts.append(f"{update_count} updated")
    if skip_count:
        parts.append(f"{skip_count} skipped")
    print(", ".join(parts) if parts else "no results")

    if failed:
        raise typer.Exit(1)


# ── diff ────────────────────────────────────────────────────────────────


def incident_key(incident: dict[str, Any]) -> tuple[str, ...]:
    loc = incident.get("codeLocation", {})
    start = loc.get("startPosition", {})
    return (
        incident.get("fileURI", ""),
        start.get("line", 0),
        start.get("character", 0),
    )


def diff_step(
    left_data: dict[str, Any],
    right_data: dict[str, Any],
) -> dict[str, Any]:
    left_incidents = left_data.get("incidents", [])
    right_incidents = right_data.get("incidents", [])

    def _key(inc: dict[str, Any]) -> tuple[str, ...]:
        return incident_key(inc)

    left_by_key: dict[tuple[str, ...], dict[str, Any]] = {}
    for inc in left_incidents:
        left_by_key[_key(inc)] = inc

    right_by_key: dict[tuple[str, ...], dict[str, Any]] = {}
    for inc in right_incidents:
        right_by_key[_key(inc)] = inc

    left_keys = set(left_by_key.keys())
    right_keys = set(right_by_key.keys())

    common = left_keys & right_keys
    left_only_keys = left_keys - right_keys
    right_only_keys = right_keys - left_keys

    left_only = sorted(
        [left_by_key[k] for k in left_only_keys],
        key=_key,
    )
    right_only = sorted(
        [right_by_key[k] for k in right_only_keys],
        key=_key,
    )

    return {
        "left_count": len(left_incidents),
        "right_count": len(right_incidents),
        "common_count": len(common),
        "left_only_count": len(left_only),
        "right_only_count": len(right_only),
        "left_only": left_only,
        "right_only": right_only,
    }


@app.command()
def diff(
    left: Annotated[Path, typer.Argument(help="Path to left result directory")],
    right: Annotated[Path, typer.Argument(help="Path to right result directory")],
    output: Annotated[Optional[Path], typer.Option(help="Output directory for diff files")] = None,
    project: Annotated[Optional[list[str]], typer.Option(help="Diff only named project(s)")] = None,
) -> None:
    """Compare two test result directories."""
    left_resolved = left.resolve()
    right_resolved = right.resolve()

    if not left_resolved.is_dir():
        print(f"ERROR: {left_resolved} is not a directory")
        raise typer.Exit(1)
    if not right_resolved.is_dir():
        print(f"ERROR: {right_resolved} is not a directory")
        raise typer.Exit(1)

    left_name = left_resolved.parent.name + "-" + left_resolved.name
    right_name = right_resolved.parent.name + "-" + right_resolved.name
    output_dir = output or RESULTS / "diff" / f"{left_name}-vs-{right_name}"
    output_dir.mkdir(parents=True, exist_ok=True)

    left_projects = {d.name for d in left_resolved.iterdir() if d.is_dir()}
    right_projects = {d.name for d in right_resolved.iterdir() if d.is_dir()}
    all_projects = sorted(left_projects | right_projects)

    if project:
        all_projects = [p for p in all_projects if p in project]

    summary: dict[str, dict[str, Any]] = {}

    for proj in all_projects:
        left_dir = left_resolved / proj
        right_dir = right_resolved / proj

        if not left_dir.is_dir():
            print(f"  {proj}: only in right")
            continue
        if not right_dir.is_dir():
            print(f"  {proj}: only in left")
            continue

        left_files = {
            f.stem.removesuffix(".result"): f
            for f in left_dir.glob("*.result.json")
        }
        right_files = {
            f.stem.removesuffix(".result"): f
            for f in right_dir.glob("*.result.json")
        }
        all_steps = sorted(set(left_files.keys()) | set(right_files.keys()))

        project_summary: dict[str, Any] = {}
        project_dir = output_dir / proj
        project_dir.mkdir(parents=True, exist_ok=True)

        for step in all_steps:
            if step == "init":
                continue

            left_file = left_files.get(step)
            right_file = right_files.get(step)

            if not left_file:
                print(f"  {proj}/{step}: only in right")
                continue
            if not right_file:
                print(f"  {proj}/{step}: only in left")
                continue

            with open(left_file) as f:
                left_data: dict[str, Any] = json.load(f)
            with open(right_file) as f:
                right_data: dict[str, Any] = json.load(f)

            result = diff_step(left_data, right_data)

            diff_file = project_dir / f"{step}.diff.json"
            diff_file.write_text(json.dumps(result, indent=2))

            project_summary[step] = {
                "left_count": result["left_count"],
                "right_count": result["right_count"],
                "common_count": result["common_count"],
                "left_only_count": result["left_only_count"],
                "right_only_count": result["right_only_count"],
            }

            lo = result["left_only_count"]
            ro = result["right_only_count"]
            c = result["common_count"]
            match_status = "MATCH" if lo == 0 and ro == 0 else "DIFF"
            print(
                f"  {proj}/{step}: {match_status} "
                f"(common={c}, left_only={lo}, right_only={ro})"
            )

        summary[proj] = project_summary

    summary_file = output_dir / "summary.json"
    summary_file.write_text(json.dumps(summary, indent=2))

    print(f"\nDiff output: {output_dir}")
    print(f"Summary:     {summary_file}")


# ── entry point ─────────────────────────────────────────────────────────

if __name__ == "__main__":
    app()
