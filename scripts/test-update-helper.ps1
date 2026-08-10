# FrameAnchor portable update helper - isolated behavioral tests
# Usage: powershell -ExecutionPolicy Bypass -File scripts\test-update-helper.ps1
# Scenarios: A (success), B (post-backup failure), C (log truncation)

param(
    [switch]$SkipCleanup
)

$ErrorActionPreference = "Stop"
$TestRoot = Join-Path $env:TEMP "fa_helper_test_$(Get-Date -Format 'yyyyMMdd_HHmmss')"
$Results = @()
$utf8Bom = [byte[]]@(0xEF, 0xBB, 0xBF)
$markerName = ".frameanchor-portable"

function Assert-Condition {
    param([string]$Test, [bool]$Condition, [string]$Detail)
    if ($Condition) {
        Write-Host "  PASS: $Test" -ForegroundColor Green
        $script:Results += @{ Test = $Test; Pass = $true; Detail = $Detail }
    } else {
        Write-Host "  FAIL: $Test - $Detail" -ForegroundColor Red
        $script:Results += @{ Test = $Test; Pass = $false; Detail = $Detail }
    }
}

function New-FakeExe {
    param([string]$Path, [string]$Content, [string]$SourceExe)
    $dir = Split-Path $Path -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    # Use where.exe (exits immediately, non-interactive) as default stub
    $src = if ($SourceExe) { $SourceExe } else { "$env:SystemRoot\System32\where.exe" }
    Copy-Item $src $Path -Force
    Write-Host "  created: $Path"
}

function New-TextFile {
    param([string]$Path, [string]$Content)
    $dir = Split-Path $Path -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    [System.IO.File]::WriteAllText($Path, $Content, [System.Text.Encoding]::UTF8)
    Write-Host "  created: $Path"
}

function Write-ScriptBom {
    param([string]$Path, [string]$Content)
    $scriptBytes = $utf8Bom + [System.Text.Encoding]::UTF8.GetBytes($Content)
    [System.IO.File]::WriteAllBytes($Path, $scriptBytes)
}

# Build the helper script template string. Paths are substituted via string
# interpolation in the double-quoted here-string; script-internal $vars are
# escaped with backtick so they pass through as literal dollar signs.
function Build-Script {
    param([string]$Old, [string]$New, [string]$Marker, [string]$Log, [int]$TargetPid)
    return @"
# FrameAnchor portable update helper
param(
    [int]`$TargetPid = $TargetPid
)

`$ErrorActionPreference = "Stop"
`$LogFile = '$Log'

# Truncate log on each invocation
Remove-Item `$LogFile -Force -ErrorAction SilentlyContinue

function Write-Log {
    param([string]`$Message)
    `$ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    "`$ts `$Message" | Out-File -FilePath `$LogFile -Append -Encoding utf8
}

Write-Log "helper started, target PID=`$TargetPid"

`$OldExe = '$Old'
`$NewExe = '$New'
`$Marker = '$Marker'
`$OldDir = Split-Path `$OldExe -Parent

Write-Log "old=`$OldExe, new=`$NewExe, marker=`$Marker"

# Wait for FrameAnchor to exit (with timeout)
`$timeout = Get-Date
while (`$true) {
    `$proc = Get-Process -Id `$TargetPid -ErrorAction SilentlyContinue
    if (-not `$proc) { break }
    if (((Get-Date) - `$timeout).TotalSeconds -gt 30) {
        Write-Log "ERROR: timeout waiting for PID `$TargetPid"
        exit 1
    }
    Start-Sleep -Milliseconds 200
}

Write-Log "target exited, waiting for file unlock"
Start-Sleep -Milliseconds 500

# Backup, replace, marker, cleanup all inside try/catch
`$Backup = "`$OldExe.bak"
try {
    # Backup old exe
    Write-Log "creating backup: `$Backup"
    Copy-Item -Path `$OldExe -Destination `$Backup -Force -ErrorAction Stop
    Write-Log "backup OK"

    # Replace with new exe (move is closer to atomic than copy+delete)
    Write-Log "replacing exe"
    Move-Item -Path `$NewExe -Destination `$OldExe -Force -ErrorAction Stop
    Write-Log "replace OK"

    # Copy marker
    if (Test-Path `$Marker) {
        Write-Log "copying marker"
        Copy-Item -Path `$Marker -Destination (Join-Path `$OldDir "$markerName") -Force
        Remove-Item -Path `$Marker -Force -ErrorAction SilentlyContinue
        Write-Log "marker OK"
    }

    # Clean up backup
    Write-Log "removing backup"
    Remove-Item -Path `$Backup -Force -ErrorAction SilentlyContinue
    Write-Log "backup cleaned"
} catch {
    Write-Log "ERROR: `$(`$_.Exception.Message)"
    # Backup exists = mutation occurred, restore; no backup = old exe untouched
    if (Test-Path `$Backup) {
        Write-Log "restoring from backup"
        Move-Item -Path `$Backup -Destination `$OldExe -Force -ErrorAction SilentlyContinue
        Write-Log "restarting original"
        Start-Process -FilePath `$OldExe
        Write-Log "original restart initiated"
    } else {
        Write-Log "ERROR: failure before backup, old exe untouched"
    }
    exit 1
}

# Success: restart
Write-Log "SUCCESS, restarting `$OldExe"
Start-Process -FilePath `$OldExe
Write-Log "restart initiated"
"@
}

# ==================================================================
# Scenario A: Success
# ==================================================================

Write-Host "`n=== Scenario A: Success ===" -ForegroundColor Cyan

$dirA = Join-Path $TestRoot "A_success"
$tmpA = Join-Path $dirA "temp"
New-Item -ItemType Directory -Path $dirA -Force | Out-Null
New-Item -ItemType Directory -Path $tmpA -Force | Out-Null

# Production layout: old exe at install dir, new exe + marker in temp
$oldA = Join-Path $dirA "FrameAnchor.exe"
$newA = Join-Path $tmpA "FrameAnchor_new.exe"
$markerA = Join-Path $tmpA $markerName
$logA = Join-Path $tmpA "update.log"

# Use non-interactive executables as stubs so Start-Process doesn't block
$srcOldExe = "$env:SystemRoot\System32\where.exe"
$srcNewExe = "$env:SystemRoot\System32\whoami.exe"

New-FakeExe $oldA "OLD" -SourceExe $srcOldExe
New-FakeExe $newA "NEW" -SourceExe $srcNewExe
New-TextFile $markerA "marker-data"
$hashOldA = (Get-FileHash $oldA -Algorithm SHA256).Hash
$hashNewA = (Get-FileHash $newA -Algorithm SHA256).Hash

$scriptA = Build-Script -Old $oldA -New $newA -Marker $markerA -Log $logA -TargetPid 999999
$scriptPathA = Join-Path $dirA "update.ps1"
Write-ScriptBom $scriptPathA $scriptA

Write-Host "  Running helper script..."
$procA = Start-Process -FilePath "powershell" -ArgumentList "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File", $scriptPathA -Wait -NoNewWindow -PassThru

$backupA = "$oldA.bak"
Assert-Condition "A1: old exe replaced (hash match)" `
    ((Get-FileHash $oldA -Algorithm SHA256).Hash -eq $hashNewA) `
    "Old hash: $((Get-FileHash $oldA -Algorithm SHA256).Hash), expected: $hashNewA"

Assert-Condition "A2: backup removed" (-not (Test-Path $backupA))

Assert-Condition "A3: marker copied" `
    ((Test-Path (Join-Path $dirA $markerName)) -and (Get-Content (Join-Path $dirA $markerName) -Raw).Trim() -eq "marker-data")

Assert-Condition "A4: temp marker removed" (-not (Test-Path $markerA))

Assert-Condition "A5: log contains SUCCESS" `
    ((Test-Path $logA) -and ((Get-Content $logA -Raw) -match "SUCCESS"))

Assert-Condition "A6: log contains restart initiated" `
    ((Test-Path $logA) -and ((Get-Content $logA -Raw) -match "restart initiated"))

Assert-Condition "A7: exit code 0" ($procA.ExitCode -eq 0) "Got: $($procA.ExitCode)"

# ==================================================================
# Scenario B: Post-backup failure (Move-Item fails after backup)
# ==================================================================

Write-Host "`n=== Scenario B: Post-backup failure ===" -ForegroundColor Cyan

$dirB = Join-Path $TestRoot "B_failure"
$tmpB = Join-Path $dirB "temp"
New-Item -ItemType Directory -Path $dirB -Force | Out-Null
New-Item -ItemType Directory -Path $tmpB -Force | Out-Null

$oldB = Join-Path $dirB "FrameAnchor.exe"
$newB = Join-Path $tmpB "FrameAnchor_new.exe"
$markerB = Join-Path $tmpB $markerName
$logB = Join-Path $tmpB "update.log"

# Backup copies old exe (file, real cmd.exe) successfully.
# But new exe path is a directory, so Move-Item file->dir fails.
New-FakeExe $oldB "ORIGINAL" -SourceExe $srcOldExe
$hashOriginalB = (Get-FileHash $oldB -Algorithm SHA256).Hash
New-Item -ItemType Directory -Path $newB -Force | Out-Null

$scriptB = Build-Script -Old $oldB -New $newB -Marker $markerB -Log $logB -TargetPid 999999
$scriptPathB = Join-Path $dirB "update.ps1"
Write-ScriptBom $scriptPathB $scriptB

Write-Host "  Running helper script (expecting Move-Item failure)..."
$procB = Start-Process -FilePath "powershell" -ArgumentList "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File", $scriptPathB -Wait -NoNewWindow -PassThru

$backupB = "$oldB.bak"
$oldExists = Test-Path $oldB
$oldIsFile = if ($oldExists) { (Get-Item $oldB) -is [System.IO.FileInfo] } else { $false }

Assert-Condition "B1: old exe restored as file" `
    ($oldIsFile) "Old: $(if ($oldExists) { (Get-Item $oldB).GetType().Name } else { 'missing' })"

Assert-Condition "B2: content preserved (hash match)" `
    ((Get-FileHash $oldB -Algorithm SHA256).Hash -eq $hashOriginalB) `
    "Old hash: $((Get-FileHash $oldB -Algorithm SHA256).Hash), expected: $hashOriginalB"

Assert-Condition "B3: backup cleaned after restore" (-not (Test-Path $backupB))

Assert-Condition "B4: log has ERROR" `
    ((Test-Path $logB) -and ((Get-Content $logB -Raw) -match "ERROR"))

Assert-Condition "B5: log has restoring from backup" `
    ((Test-Path $logB) -and ((Get-Content $logB -Raw) -match "restoring from backup"))

Assert-Condition "B6: exit code 1" ($procB.ExitCode -eq 1) "Got: $($procB.ExitCode)"

Assert-Condition "B7: log has original restart initiated" `
    ((Test-Path $logB) -and ((Get-Content $logB -Raw) -match "original restart initiated"))

# ==================================================================
# Scenario C: Log truncation
# ==================================================================

Write-Host "`n=== Scenario C: Log truncation ===" -ForegroundColor Cyan

$dirC = Join-Path $TestRoot "C_log_truncation"
$tmpC = Join-Path $dirC "temp"
New-Item -ItemType Directory -Path $dirC -Force | Out-Null
New-Item -ItemType Directory -Path $tmpC -Force | Out-Null

$oldC = Join-Path $dirC "FrameAnchor.exe"
$newC = Join-Path $tmpC "FrameAnchor_new.exe"
$markerC = Join-Path $tmpC $markerName
$logC = Join-Path $tmpC "update.log"

# Pre-populate with stale content (simulate multiple prior runs)
$staleContent = (1..500 | ForEach-Object { "STALE_LOG_LINE_$_" }) -join "`r`n"
[System.IO.File]::WriteAllText($logC, $staleContent, [System.Text.Encoding]::UTF8)
$staleSize = (Get-Item $logC).Length

New-FakeExe $oldC "OLD" -SourceExe $srcOldExe
New-FakeExe $newC "NEW" -SourceExe $srcNewExe
New-TextFile $markerC "marker"

$scriptC = Build-Script -Old $oldC -New $newC -Marker $markerC -Log $logC -TargetPid 999999
$scriptPathC = Join-Path $dirC "update.ps1"
Write-ScriptBom $scriptPathC $scriptC

Write-Host "  Running helper script..."
$procC = Start-Process -FilePath "powershell" -ArgumentList "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File", $scriptPathC -Wait -NoNewWindow -PassThru

$newSize = (Get-Item $logC).Length
$logContent = Get-Content $logC -Raw

Assert-Condition "C1: log truncated ($newSize < $staleSize)" ($newSize -lt $staleSize)
Assert-Condition "C2: no stale lines" ($logContent -notmatch "STALE_LOG_LINE")
Assert-Condition "C3: has current markers" ($logContent -match "helper started" -and $logContent -match "SUCCESS")
Assert-Condition "C4: exit code 0" ($procC.ExitCode -eq 0) "Got: $($procC.ExitCode)"

# ==================================================================
# Summary
# ==================================================================

$passCount = ($Results | Where-Object { $_.Pass }).Count
$failCount = ($Results | Where-Object { -not $_.Pass }).Count
$totalCount = $Results.Count

Write-Host "`n========================================" -ForegroundColor White
Write-Host "  Behavioral Test Results" -ForegroundColor White
Write-Host "========================================" -ForegroundColor White
Write-Host "  Total: $totalCount  Pass: $passCount  Fail: $failCount" -ForegroundColor $(if ($failCount -eq 0) { "Green" } else { "Red" })

if ($failCount -gt 0) {
    Write-Host "`n  Failures:" -ForegroundColor Red
    $Results | Where-Object { -not $_.Pass } | ForEach-Object {
        Write-Host "    $($_.Test): $($_.Detail)" -ForegroundColor Red
    }
}

if (-not $SkipCleanup) {
    Remove-Item -Path $TestRoot -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "`n  Cleaned: $TestRoot"
} else {
    Write-Host "`n  Kept: $TestRoot"
}

if ($failCount -gt 0) { exit 1 }
Write-Host "`n  All behavioral tests passed.`n"
exit 0
