# FrameAnchor DeepSec security scan wrapper
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\deepsec-scan.ps1
#
# Why not scan the repository root directly? DeepSec does not read .gitignore,
# so a direct scan would also crawl src-tauri/target (generated docs/build
# output) and .claude/worktrees (working copies), producing noise findings in
# files that are not first-party source. This wrapper stages only Git-tracked
# files (`git ls-files`) into a throwaway directory, scans that, and cleans up.

param()

$ErrorActionPreference = "Stop"

# Resolve the repository root so the script runs from any directory inside it.
$repoRoot = (& git rev-parse --show-toplevel 2>$null)
if (-not $repoRoot) {
    throw "Not inside a Git repository."
}

# Unique staging directory in the system temp location.
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("deepsec-scan-" + [guid]::NewGuid().ToString("N"))
$sep = [System.IO.Path]::DirectorySeparatorChar

$exitCode = 0
try {
    New-Item -ItemType Directory -Path $staging -Force | Out-Null

    # Stage only Git-tracked first-party files. `git ls-files` already omits
    # untracked artifacts and everything covered by .gitignore (target/, .claude/,
    # node_modules/, dist/). The explicit filter is defence-in-depth against any
    # tracked copy that happens to live under those paths.
    $files = & git -C $repoRoot ls-files
    foreach ($file in $files) {
        if ([string]::IsNullOrWhiteSpace($file)) { continue }
        $rel = $file.Trim().Replace("/", $sep)
        if ($rel.StartsWith(".claude" + $sep) -or $rel.StartsWith("target" + $sep)) {
            continue
        }

        $dest = Join-Path $staging $rel
        $destDir = Split-Path -Parent $dest
        if ($destDir) {
            New-Item -ItemType Directory -Path $destDir -Force | Out-Null
        }
        Copy-Item -LiteralPath (Join-Path $repoRoot $rel) -Destination $dest -Force
    }

    # Scan the staging tree. Default to no remote L3 (off by default anyway).
    & deepsec shield scan $staging --no-remote-l3
    $exitCode = $LASTEXITCODE
}
finally {
    # Always remove the staging tree, even if staging or the scan failed.
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }
}

exit $exitCode
