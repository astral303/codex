use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

const PREEXISTING_SCROLLBACK_COUNT: usize = 100;
const TERMINAL_COLUMNS: u16 = 120;
const TERMINAL_ROWS: u16 = 30;
const WRAPPED_LAUNCH_MARKER: &str = "WRAPPED-CODEX-LAUNCH";
const WORKING_PROMPT_START_MARKER: &str = "WORKING-PROMPT-START";
const WORKING_PROMPT_END_MARKER: &str = "WORKING-PROMPT-END";
const WORKING_STATUS_MARKER: &str = "Working (";
const TOOL_PREAMBLE_MARKER: &str = "TOOL-PREAMBLE-BEFORE-CALL";
const TOOL_AFTER_PREAMBLE_MARKER: &str = "TOOL-AFTER-PREAMBLE-CALL";
const LONG_HISTORY_ANCHOR_MARKER: &str = "DELIVERABLES-ANCHOR";
const LONG_DRAFT_START_MARKER: &str = "LONG-DRAFT-START";
const LONG_DRAFT_END_MARKER: &str = "LONG-DRAFT-END";
const LONG_DRAFT_SUBMITTED_MARKER: &str = "LONG-DRAFT-SUBMITTED";
const PENDING_INPUT_START_MARKER: &str = "PENDING-INPUT-START";
const PENDING_INPUT_END_MARKER: &str = "PENDING-INPUT-END";
const PENDING_STATUS_START_MARKER: &str = "Messages to be submitted after next tool call";
const PENDING_STATUS_END_MARKER: &str = "send immediately)";
const SETTLED_MCP_HISTORY_MARKER: &str = "MCP startup incomplete (failed: slow_startup)";
const TOOL_ROW_DELAY_MS: u64 = 200;
const MAX_SETTLED_LAUNCH_HEADER_GAP: usize = 12;
const MAX_SETTLED_HISTORY_COMPOSER_GAP: usize = 2;
const MAX_TRANSCRIPT_BLANK_ROW_RUN: usize = 2;

#[derive(Clone, Copy)]
enum ReflowResponseScenario {
    FinalAnswer {
        sentinel_count: usize,
    },
    PreambleThenShellTool {
        sentinel_count: usize,
    },
    LongComposer {
        sentinel_count: usize,
    },
    ShellToolBatches {
        batch_count: usize,
        calls_per_batch: usize,
        sentinel_count: usize,
    },
}

impl ReflowResponseScenario {
    fn sentinel_count(self) -> usize {
        match self {
            Self::FinalAnswer { sentinel_count }
            | Self::PreambleThenShellTool { sentinel_count }
            | Self::LongComposer { sentinel_count }
            | Self::ShellToolBatches { sentinel_count, .. } => sentinel_count,
        }
    }

    fn expected_request_count(self) -> usize {
        match self {
            Self::FinalAnswer { .. } => 1,
            Self::PreambleThenShellTool { .. } => 2,
            Self::LongComposer { .. } => 2,
            Self::ShellToolBatches { batch_count, .. } => batch_count + 1,
        }
    }
}

fn blank_rows_between_launch_and_header(text: &str, launch_index: usize) -> Result<usize> {
    let lines = text.lines().collect::<Vec<_>>();
    let launch_line = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("CODEX-RELAUNCH-PROMPT>&"))
        .nth(launch_index)
        .map(|(index, _)| index)
        .with_context(|| format!("missing launch command at index {launch_index}"))?;
    let header_title_line = lines
        .iter()
        .enumerate()
        .skip(launch_line + 1)
        .find(|(_, line)| line.contains("OpenAI Codex"))
        .map(|(index, _)| index)
        .context("missing session header after launch command")?;
    let header_border_line = header_title_line
        .checked_sub(1)
        .context("session header title has no top border")?;
    if header_border_line <= launch_line {
        bail!(
            "session header top border overlaps the launch command at line {}",
            launch_line + 1
        );
    }
    let gap = &lines[launch_line + 1..header_border_line];
    if let Some(line) = gap.iter().find(|line| !line.trim().is_empty()) {
        bail!("unexpected content between launch command and session header: {line:?}");
    }
    Ok(gap.len())
}

fn blank_rows_between_history_and_composer(text: &str, history_marker: &str) -> Result<usize> {
    let lines = text.lines().collect::<Vec<_>>();
    let history_line = lines
        .iter()
        .rposition(|line| line.contains(history_marker))
        .with_context(|| format!("missing settled history marker {history_marker:?}"))?;
    let composer_line = lines
        .iter()
        .enumerate()
        .skip(history_line + 1)
        .find(|(_, line)| line.trim_start().starts_with('›'))
        .map(|(index, _)| index)
        .context("missing composer after settled history")?;
    let gap = &lines[history_line + 1..composer_line];
    if let Some(line) = gap.iter().find(|line| !line.trim().is_empty()) {
        bail!("unexpected content between settled history and composer: {line:?}");
    }
    Ok(gap.len())
}

fn assert_numbered_markers_are_contiguous(text: &str, prefix: &str, count: usize) -> Result<()> {
    let lines = text.lines().collect::<Vec<_>>();
    let marker_lines = (1..=count)
        .map(|index| {
            let marker = format!("{prefix}-{index:03}");
            lines
                .iter()
                .position(|line| line.contains(&marker))
                .with_context(|| format!("missing marker {marker:?}"))
        })
        .collect::<Result<Vec<_>>>()?;

    for (index, pair) in marker_lines.windows(2).enumerate() {
        if pair[1] != pair[0] + 1 {
            bail!(
                "blank or unrelated rows split {prefix}-{:03} and {prefix}-{:03}",
                index + 1,
                index + 2,
            );
        }
    }
    Ok(())
}

fn assert_session_headers_have_no_blank_interior(text: &str) -> Result<()> {
    let lines = text.lines().collect::<Vec<_>>();
    for title_line in lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("OpenAI Codex"))
        .map(|(index, _)| index)
    {
        let bottom_border = lines
            .iter()
            .enumerate()
            .skip(title_line + 1)
            .find(|(_, line)| line.trim_start().starts_with('╰'))
            .map(|(index, _)| index)
            .context("missing session header bottom border")?;
        if lines[title_line + 1..bottom_border]
            .iter()
            .any(|line| line.trim().is_empty())
        {
            bail!("blank terminal rows split a session header at line {title_line}");
        }
    }
    Ok(())
}

fn max_blank_row_run_after_session_header(text: &str) -> Result<usize> {
    let lines = text.lines().collect::<Vec<_>>();
    let header = lines
        .iter()
        .position(|line| line.contains("OpenAI Codex"))
        .context("missing session header")?;
    let last_content = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .context("terminal capture contains no content")?;
    if header >= last_content {
        return Ok(0);
    }

    let mut longest = 0;
    let mut current = 0;
    for line in &lines[header + 1..=last_content] {
        if line.trim().is_empty() {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    Ok(longest)
}

const WINDOWS_TERMINAL_DRIVER: &str = r#"
param(
    [Parameter(Mandatory = $true)]
    [string] $WtPath,

    [Parameter(Mandatory = $true)]
    [string] $PwshPath,

    [Parameter(Mandatory = $true)]
    [string] $LauncherPath,

    [Parameter(Mandatory = $true)]
    [string] $WindowTitle,

    [Parameter(Mandatory = $true)]
    [string] $CaptureDirectory,

    [Parameter(Mandatory = $true)]
    [string] $ConsoleProcessPath,

    [Parameter(Mandatory = $true)]
    [string] $McpSignalPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupReadyPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupCapturedPath,

    [Parameter(Mandatory = $true)]
    [string] $InputCompletePath,

    [Parameter(Mandatory = $true)]
    [string] $FinalSentinel,

    [string] $StartupCaptureNeedle = '',

    [string] $WorkingInput = '',

    [string] $WorkingCaptureNeedle = '',

    [string] $PendingInput = '',

    [string] $PendingCaptureNeedle = '',

    [string] $LongDraftInput = '',

    [string] $LongDraftCaptureNeedle = '',

    [string] $LongDraftAnchor = '',

    [string] $LongDraftSubmittedNeedle = '',

    [Parameter(Mandatory = $true)]
    [int] $Columns,

    [Parameter(Mandatory = $true)]
    [int] $Rows
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CodexTerminalInput {
    public const uint ShiftPressed = 0x0010;

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AttachConsole(uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flags,
        IntPtr templateFile
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr objectHandle);

    [DllImport("kernel32.dll", EntryPoint = "WriteConsoleInputW", SetLastError = true)]
    static extern bool WriteConsoleInput(
        IntPtr consoleInput,
        CONSOLE_INPUT_RECORD[] records,
        uint recordCount,
        out uint recordsWritten
    );

    static IntPtr consoleInput = new IntPtr(-1);

    [StructLayout(LayoutKind.Explicit)]
    struct CONSOLE_INPUT_RECORD {
        [FieldOffset(0)] public ushort eventType;
        [FieldOffset(4)] public CONSOLE_KEY_EVENT keyEvent;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct CONSOLE_KEY_EVENT {
        [MarshalAs(UnmanagedType.Bool)]
        public bool keyDown;
        public ushort repeatCount;
        public ushort virtualKey;
        public ushort scanCode;
        public char unicodeChar;
        public uint controlKeyState;
    }

    public static void ConnectConsole(uint processId) {
        DisconnectConsole();
        if (!AttachConsole(processId)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        consoleInput = CreateFile(
            "CONIN$",
            0x40000000,
            0x00000003,
            IntPtr.Zero,
            3,
            0,
            IntPtr.Zero
        );
        if (consoleInput == new IntPtr(-1)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    public static void DisconnectConsole() {
        if (consoleInput != new IntPtr(-1)) {
            CloseHandle(consoleInput);
            consoleInput = new IntPtr(-1);
        }
        FreeConsole();
    }

    public static void SendText(string text) {
        foreach (var character in text) {
            SendConsoleKey(0, character, 0);
        }
    }

    public static void SendVirtualKey(ushort virtualKey) {
        var unicodeChar = virtualKey == 0x0D
            ? '\r'
            : virtualKey == 0x08
                ? '\b'
                : '\0';
        SendConsoleKey(virtualKey, unicodeChar, 0);
    }

    public static void SendModifiedVirtualKey(ushort virtualKey, uint controlKeyState) {
        var unicodeChar = virtualKey == 0x0D ? '\r' : '\0';
        SendConsoleKey(virtualKey, unicodeChar, controlKeyState);
    }

    static void SendConsoleKey(ushort virtualKey, char unicodeChar, uint controlKeyState) {
        var records = new[] {
            ConsoleKeyRecord(true, virtualKey, unicodeChar, controlKeyState),
            ConsoleKeyRecord(false, virtualKey, unicodeChar, controlKeyState),
        };
        if (!WriteConsoleInput(consoleInput, records, (uint) records.Length, out var written)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        if (written != records.Length) {
            throw new System.ComponentModel.Win32Exception("Incomplete console input write.");
        }
    }

    static CONSOLE_INPUT_RECORD ConsoleKeyRecord(
        bool keyDown,
        ushort virtualKey,
        char unicodeChar,
        uint controlKeyState
    ) {
        return new CONSOLE_INPUT_RECORD {
            eventType = 1,
            keyEvent = new CONSOLE_KEY_EVENT {
                keyDown = keyDown,
                repeatCount = 1,
                virtualKey = virtualKey,
                unicodeChar = unicodeChar,
                controlKeyState = controlKeyState,
            },
        };
    }

}
'@

function Find-TerminalWindow {
    param(
        [string] $Title,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $nameCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        $Title
    )

    while ([DateTime]::UtcNow -lt $deadline) {
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition
        )

        foreach ($window in $windows) {
            try {
                if ($window.Current.ClassName -ne 'CASCADIA_HOSTING_WINDOW_CLASS') {
                    continue
                }

                $titleElement = $window.FindFirst(
                    [System.Windows.Automation.TreeScope]::Descendants,
                    $nameCondition
                )
                if ($null -ne $titleElement) {
                    return $window
                }
            } catch {
                # A window can disappear while its automation properties are being queried.
            }
        }

        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for Windows Terminal window '$Title'."
}

function Find-TerminalControl {
    param(
        [System.Windows.Automation.AutomationElement] $Window
    )

    $classCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty,
        'TermControl'
    )
    $terminal = $Window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        $classCondition
    )
    if ($null -eq $terminal) {
        throw 'Windows Terminal did not expose a TermControl automation element.'
    }
    return $terminal
}

function Connect-TerminalConsole {
    param(
        [string] $ProcessPath
    )

    $processDeadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(30)
    while (-not (Test-Path -LiteralPath $ProcessPath)) {
        if ([DateTime]::UtcNow -ge $processDeadline) {
            throw "Timed out waiting for console process '$ProcessPath'."
        }
        Start-Sleep -Milliseconds 100
    }

    $consoleProcessId = [uint32] [System.IO.File]::ReadAllText($ProcessPath)
    [CodexTerminalInput]::ConnectConsole($consoleProcessId)
}

function Get-TerminalText {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal
    )

    $patternObject = $null
    if (-not $Terminal.TryGetCurrentPattern(
        [System.Windows.Automation.TextPattern]::Pattern,
        [ref] $patternObject
    )) {
        throw 'Windows Terminal TermControl does not expose TextPattern.'
    }

    $textPattern = [System.Windows.Automation.TextPattern] $patternObject
    return $textPattern.DocumentRange.GetText(-1)
}

function Scroll-TerminalTextIntoView {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal,
        [string] $Needle
    )

    $patternObject = $null
    if (-not $Terminal.TryGetCurrentPattern(
        [System.Windows.Automation.TextPattern]::Pattern,
        [ref] $patternObject
    )) {
        throw 'Windows Terminal TermControl does not expose TextPattern.'
    }

    $textPattern = [System.Windows.Automation.TextPattern] $patternObject
    $range = $textPattern.DocumentRange.FindText($Needle, $false, $true)
    if ($null -eq $range) {
        throw "Windows Terminal text does not contain '$Needle'."
    }
    $range.ScrollIntoView($true)
}

function Wait-ForTerminalText {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal,
        [string] $Needle,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $lastText = ''
    while ([DateTime]::UtcNow -lt $deadline) {
        $lastText = Get-TerminalText -Terminal $Terminal
        if ($lastText.Contains($Needle)) {
            return $lastText
        }
        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for terminal text '$Needle'. Last text:`n$lastText"
}

function Wait-ForTerminalTextWithoutStrayPrompt {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal,
        [string] $Needle,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $lastText = ''
    while ([DateTime]::UtcNow -lt $deadline) {
        $lastText = Get-TerminalText -Terminal $Terminal
        $lines = $lastText -split "\r?\n"
        $strayPromptRows = @(
            for ($index = 0; $index -lt $lines.Length; $index++) {
                if ($lines[$index].Trim() -eq '›') {
                    $index + 1
                }
            }
        )
        if ($strayPromptRows.Count -gt 0) {
            Save-TerminalText -Name 'stray-prompt' -Text $lastText
            throw "Found stray prompt glyphs on terminal rows $($strayPromptRows -join ', ')."
        }
        if ($lastText.Contains($Needle)) {
            return $lastText
        }
        Start-Sleep -Milliseconds 20
    }

    throw "Timed out waiting for terminal text '$Needle'. Last text:`n$lastText"
}

function Save-TerminalText {
    param(
        [string] $Name,
        [string] $Text
    )

    $path = Join-Path $CaptureDirectory "$Name.txt"
    [System.IO.File]::WriteAllText($path, $Text)
}

& $WtPath `
    --window new `
    --size "$Columns,$Rows" `
    new-tab `
    --title $WindowTitle `
    $PwshPath `
    -NoLogo `
    -NoExit `
    -File $LauncherPath
if ($LASTEXITCODE -ne 0) {
    throw "wt.exe failed with exit code $LASTEXITCODE."
}

$window = $null
$windowPattern = $null
try {
    $window = Find-TerminalWindow `
        -Title $WindowTitle `
        -Timeout ([TimeSpan]::FromSeconds(30))
    $terminal = Find-TerminalControl -Window $window

    $windowPatternObject = $null
    if (-not $window.TryGetCurrentPattern(
        [System.Windows.Automation.WindowPattern]::Pattern,
        [ref] $windowPatternObject
    )) {
        throw 'Windows Terminal window does not expose WindowPattern.'
    }
    $windowPattern = [System.Windows.Automation.WindowPattern] $windowPatternObject

    if (-not [string]::IsNullOrEmpty($StartupCaptureNeedle)) {
        $startup = Wait-ForTerminalText `
            -Terminal $terminal `
            -Needle $StartupCaptureNeedle `
            -Timeout ([TimeSpan]::FromSeconds(30))
        Save-TerminalText `
            -Name 'startup' `
            -Text $startup
    }

    if (-not [string]::IsNullOrEmpty($WorkingInput)) {
        Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
        try {
            foreach ($character in $WorkingInput.ToCharArray()) {
                [CodexTerminalInput]::SendText([string] $character)
                Start-Sleep -Milliseconds 10
            }
            Start-Sleep -Milliseconds 250
            [CodexTerminalInput]::SendVirtualKey(0x0D)
        } finally {
            [CodexTerminalInput]::DisconnectConsole()
        }
    }

    if (-not [string]::IsNullOrEmpty($WorkingCaptureNeedle)) {
        Wait-ForTerminalTextWithoutStrayPrompt `
            -Terminal $terminal `
            -Needle $WorkingCaptureNeedle `
            -Timeout ([TimeSpan]::FromSeconds(30)) | Out-Null
        Start-Sleep -Milliseconds 250
        $duringWorking = Get-TerminalText -Terminal $terminal
        Save-TerminalText -Name 'during-working' -Text $duringWorking
    }

    if (-not [string]::IsNullOrEmpty($PendingInput)) {
        Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
        try {
            [CodexTerminalInput]::SendText($PendingInput)
            Start-Sleep -Milliseconds 750
            [CodexTerminalInput]::SendVirtualKey(0x0D)
        } finally {
            [CodexTerminalInput]::DisconnectConsole()
        }
        Wait-ForTerminalText `
            -Terminal $terminal `
            -Needle $PendingCaptureNeedle `
            -Timeout ([TimeSpan]::FromSeconds(30)) | Out-Null
        Start-Sleep -Milliseconds 250
        Save-TerminalText `
            -Name 'during-pending-input' `
            -Text (Get-TerminalText -Terminal $terminal)
    }

    Wait-ForTerminalText `
        -Terminal $terminal `
        -Needle $FinalSentinel `
        -Timeout ([TimeSpan]::FromSeconds(120)) | Out-Null
    Start-Sleep -Milliseconds 750

    $baseline = Get-TerminalText -Terminal $terminal
    Save-TerminalText -Name 'baseline' -Text $baseline

    if (-not [string]::IsNullOrEmpty($LongDraftInput)) {
        Save-TerminalText -Name 'before-long-draft' -Text $baseline
        Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
        try {
            foreach ($character in $LongDraftInput.ToCharArray()) {
                if ($character -eq "`n") {
                    [CodexTerminalInput]::SendModifiedVirtualKey(
                        0x0D,
                        [CodexTerminalInput]::ShiftPressed
                    )
                } else {
                    [CodexTerminalInput]::SendText([string] $character)
                }
                Start-Sleep -Milliseconds 10
            }
        } finally {
            [CodexTerminalInput]::DisconnectConsole()
        }
        Wait-ForTerminalText `
            -Terminal $terminal `
            -Needle $LongDraftCaptureNeedle `
            -Timeout ([TimeSpan]::FromSeconds(30)) | Out-Null
        Start-Sleep -Milliseconds 500
        Save-TerminalText `
            -Name 'during-long-draft' `
            -Text (Get-TerminalText -Terminal $terminal)

        Scroll-TerminalTextIntoView -Terminal $terminal -Needle $LongDraftAnchor
        Start-Sleep -Milliseconds 500
        Save-TerminalText `
            -Name 'during-long-draft-scrolled' `
            -Text (Get-TerminalText -Terminal $terminal)

        Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
        try {
            [CodexTerminalInput]::SendVirtualKey(0x0D)
        } finally {
            [CodexTerminalInput]::DisconnectConsole()
        }
        Wait-ForTerminalText `
            -Terminal $terminal `
            -Needle $LongDraftSubmittedNeedle `
            -Timeout ([TimeSpan]::FromSeconds(60)) | Out-Null
        Start-Sleep -Milliseconds 750
        Save-TerminalText `
            -Name 'after-long-draft-submit' `
            -Text (Get-TerminalText -Terminal $terminal)
    }

    New-Item -ItemType File -Path $McpSignalPath | Out-Null
    $inputDeadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(60)
    while (-not (Test-Path -LiteralPath $PopupReadyPath)) {
        if ([DateTime]::UtcNow -ge $inputDeadline) {
            throw "Timed out waiting for popup readiness '$PopupReadyPath'."
        }
        Start-Sleep -Milliseconds 100
    }
    Start-Sleep -Milliseconds 500

    $duringPopup = Get-TerminalText -Terminal $terminal
    Save-TerminalText -Name 'during-popup' -Text $duringPopup
    New-Item -ItemType File -Path $PopupCapturedPath | Out-Null

    while (-not (Test-Path -LiteralPath $InputCompletePath)) {
        if ([DateTime]::UtcNow -ge $inputDeadline) {
            throw "Timed out waiting for input completion '$InputCompletePath'."
        }
        Start-Sleep -Milliseconds 100
    }
    Start-Sleep -Seconds 1

    $afterMcp = Get-TerminalText -Terminal $terminal
    Save-TerminalText -Name 'after-mcp' -Text $afterMcp

    $windowPattern.SetWindowVisualState(
        [System.Windows.Automation.WindowVisualState]::Maximized
    )
    Start-Sleep -Seconds 2
    Save-TerminalText `
        -Name 'after-maximize' `
        -Text (Get-TerminalText -Terminal $terminal)

    $windowPattern.SetWindowVisualState(
        [System.Windows.Automation.WindowVisualState]::Normal
    )
    Start-Sleep -Seconds 2
    Save-TerminalText `
        -Name 'after-restore' `
        -Text (Get-TerminalText -Terminal $terminal)
} finally {
    if ($null -ne $windowPattern) {
        try {
            $windowPattern.Close()
        } catch {
            Write-Warning "Failed to close test Windows Terminal window: $_"
        }
    }
}
"#;

const WINDOWS_TERMINAL_INPUT_HELPER: &str = r#"
param(
    [Parameter(Mandatory = $true)]
    [string] $WindowTitle,

    [Parameter(Mandatory = $true)]
    [string] $CaptureDirectory,

    [Parameter(Mandatory = $true)]
    [string] $ConsoleProcessPath,

    [Parameter(Mandatory = $true)]
    [string] $McpSignalPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupReadyPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupCapturedPath,

    [Parameter(Mandatory = $true)]
    [string] $InputCompletePath,

    [Parameter(Mandatory = $true)]
    [int] $InputCycles,

    [Parameter(Mandatory = $true)]
    [string] $FailurePath
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CodexTerminalInput {
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AttachConsole(uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flags,
        IntPtr templateFile
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr objectHandle);

    [DllImport("kernel32.dll", EntryPoint = "WriteConsoleInputW", SetLastError = true)]
    static extern bool WriteConsoleInput(
        IntPtr consoleInput,
        CONSOLE_INPUT_RECORD[] records,
        uint recordCount,
        out uint recordsWritten
    );

    static IntPtr consoleInput = new IntPtr(-1);

    [StructLayout(LayoutKind.Explicit)]
    struct CONSOLE_INPUT_RECORD {
        [FieldOffset(0)] public ushort eventType;
        [FieldOffset(4)] public CONSOLE_KEY_EVENT keyEvent;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct CONSOLE_KEY_EVENT {
        [MarshalAs(UnmanagedType.Bool)]
        public bool keyDown;
        public ushort repeatCount;
        public ushort virtualKey;
        public ushort scanCode;
        public char unicodeChar;
        public uint controlKeyState;
    }

    public static void ConnectConsole(uint processId) {
        DisconnectConsole();
        if (!AttachConsole(processId)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        consoleInput = CreateFile(
            "CONIN$",
            0x40000000,
            0x00000003,
            IntPtr.Zero,
            3,
            0,
            IntPtr.Zero
        );
        if (consoleInput == new IntPtr(-1)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    public static void DisconnectConsole() {
        if (consoleInput != new IntPtr(-1)) {
            CloseHandle(consoleInput);
            consoleInput = new IntPtr(-1);
        }
        FreeConsole();
    }

    public static void SendText(string text) {
        foreach (var character in text) {
            SendConsoleKey(0, character);
        }
    }

    public static void SendVirtualKey(ushort virtualKey) {
        var unicodeChar = virtualKey == 0x0D
            ? '\r'
            : virtualKey == 0x08
                ? '\b'
                : '\0';
        SendConsoleKey(virtualKey, unicodeChar);
    }

    static void SendConsoleKey(ushort virtualKey, char unicodeChar) {
        var records = new[] {
            ConsoleKeyRecord(true, virtualKey, unicodeChar),
            ConsoleKeyRecord(false, virtualKey, unicodeChar),
        };
        if (!WriteConsoleInput(consoleInput, records, (uint) records.Length, out var written)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        if (written != records.Length) {
            throw new System.ComponentModel.Win32Exception("Incomplete console input write.");
        }
    }

    static CONSOLE_INPUT_RECORD ConsoleKeyRecord(
        bool keyDown,
        ushort virtualKey,
        char unicodeChar
    ) {
        return new CONSOLE_INPUT_RECORD {
            eventType = 1,
            keyEvent = new CONSOLE_KEY_EVENT {
                keyDown = keyDown,
                repeatCount = 1,
                virtualKey = virtualKey,
                unicodeChar = unicodeChar,
            },
        };
    }

}
'@

function Get-TerminalText {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal
    )

    $patternObject = $null
    if (-not $Terminal.TryGetCurrentPattern(
        [System.Windows.Automation.TextPattern]::Pattern,
        [ref] $patternObject
    )) {
        throw 'Windows Terminal TermControl does not expose TextPattern.'
    }

    $textPattern = [System.Windows.Automation.TextPattern] $patternObject
    return $textPattern.DocumentRange.GetText(-1)
}

function Save-InputCapture {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal,
        [int] $Step
    )

    $name = 'input-step-{0:D3}.txt' -f $Step
    $path = Join-Path $CaptureDirectory $name
    $text = Get-TerminalText -Terminal $Terminal
    [System.IO.File]::WriteAllText($path, $text)
}

function Find-TerminalWindow {
    param(
        [string] $Title,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $nameCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        $Title
    )

    while ([DateTime]::UtcNow -lt $deadline) {
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition
        )

        foreach ($window in $windows) {
            try {
                if ($window.Current.ClassName -ne 'CASCADIA_HOSTING_WINDOW_CLASS') {
                    continue
                }

                $titleElement = $window.FindFirst(
                    [System.Windows.Automation.TreeScope]::Descendants,
                    $nameCondition
                )
                if ($null -ne $titleElement) {
                    return $window
                }
            } catch {
                # A window can disappear while its automation properties are being queried.
            }
        }

        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for Windows Terminal window '$Title'."
}

function Find-TerminalControl {
    param(
        [System.Windows.Automation.AutomationElement] $Window
    )

    $classCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty,
        'TermControl'
    )
    $terminal = $Window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        $classCondition
    )
    if ($null -eq $terminal) {
        throw 'Windows Terminal did not expose a TermControl automation element.'
    }
    return $terminal
}

function Connect-TerminalConsole {
    param(
        [string] $ProcessPath
    )

    $processDeadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(30)
    while (-not (Test-Path -LiteralPath $ProcessPath)) {
        if ([DateTime]::UtcNow -ge $processDeadline) {
            throw "Timed out waiting for console process '$ProcessPath'."
        }
        Start-Sleep -Milliseconds 100
    }

    $consoleProcessId = [uint32] [System.IO.File]::ReadAllText($ProcessPath)
    [CodexTerminalInput]::ConnectConsole($consoleProcessId)
}

try {
    $signalDeadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(60)
    while (-not (Test-Path -LiteralPath $McpSignalPath)) {
        if ([DateTime]::UtcNow -ge $signalDeadline) {
            throw "Timed out waiting for MCP signal '$McpSignalPath'."
        }
        Start-Sleep -Milliseconds 100
    }

    $window = Find-TerminalWindow `
        -Title $WindowTitle `
        -Timeout ([TimeSpan]::FromSeconds(30))
    $terminal = Find-TerminalControl -Window $window
    Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
    try {
        $inputStep = 0
        for ($cycle = 0; $cycle -lt $InputCycles; $cycle++) {
            foreach ($character in @('/', 'm', 'c', 'p')) {
                [CodexTerminalInput]::SendText($character)
                Start-Sleep -Milliseconds 500
                Save-InputCapture -Terminal $terminal -Step $inputStep
                $inputStep++
                if ($cycle -eq 0 -and $character -eq '/') {
                    New-Item -ItemType File -Path $PopupReadyPath | Out-Null
                    $captureDeadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(30)
                    while (-not (Test-Path -LiteralPath $PopupCapturedPath)) {
                        if ([DateTime]::UtcNow -ge $captureDeadline) {
                            throw "Timed out waiting for popup capture '$PopupCapturedPath'."
                        }
                        Start-Sleep -Milliseconds 100
                    }
                }
            }
            Start-Sleep -Milliseconds 750
            foreach ($character in 1..4) {
                [CodexTerminalInput]::SendVirtualKey(0x08)
                Start-Sleep -Milliseconds 500
                Save-InputCapture -Terminal $terminal -Step $inputStep
                $inputStep++
            }
            Start-Sleep -Milliseconds 750
        }
    } finally {
        [CodexTerminalInput]::DisconnectConsole()
    }
    New-Item -ItemType File -Path $InputCompletePath | Out-Null
    exit 0
} catch {
    [System.IO.File]::WriteAllText($FailurePath, $_.Exception.ToString())
    exit 1
}
"#;

const WINDOWS_TERMINAL_RELAUNCH_HELPER: &str = r#"
param(
    [Parameter(Mandatory = $true)]
    [string] $WindowTitle,

    [Parameter(Mandatory = $true)]
    [string] $ConsoleProcessPath,

    [Parameter(Mandatory = $true)]
    [string] $LaunchCommandPath,

    [Parameter(Mandatory = $true)]
    [string] $PromptMarker,

    [Parameter(Mandatory = $true)]
    [string] $McpSignalPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupReadyPath,

    [Parameter(Mandatory = $true)]
    [string] $PopupCapturedPath,

    [Parameter(Mandatory = $true)]
    [string] $InputCompletePath,

    [Parameter(Mandatory = $true)]
    [string] $FirstExitPath,

    [Parameter(Mandatory = $true)]
    [string] $SecondStartPath,

    [Parameter(Mandatory = $true)]
    [string] $SecondExitPath,

    [Parameter(Mandatory = $true)]
    [string] $FailurePath
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CodexTerminalInput {
    static IntPtr consoleInput = new IntPtr(-1);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AttachConsole(uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr objectHandle);

    [DllImport("kernel32.dll", EntryPoint = "WriteConsoleInputW", SetLastError = true)]
    static extern bool WriteConsoleInput(
        IntPtr consoleInput,
        CONSOLE_INPUT_RECORD[] records,
        uint recordCount,
        out uint written
    );

    [StructLayout(LayoutKind.Explicit)]
    struct CONSOLE_INPUT_RECORD {
        [FieldOffset(0)] public ushort eventType;
        [FieldOffset(4)] public CONSOLE_KEY_EVENT keyEvent;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct CONSOLE_KEY_EVENT {
        [MarshalAs(UnmanagedType.Bool)]
        public bool keyDown;
        public ushort repeatCount;
        public ushort virtualKey;
        public ushort scanCode;
        public char unicodeChar;
        public uint controlKeyState;
    }

    public static void ConnectConsole(uint processId) {
        DisconnectConsole();
        if (!AttachConsole(processId)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        consoleInput = CreateFile(
            "CONIN$",
            0x40000000,
            0x00000003,
            IntPtr.Zero,
            3,
            0,
            IntPtr.Zero
        );
        if (consoleInput == new IntPtr(-1)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    public static void DisconnectConsole() {
        if (consoleInput != new IntPtr(-1)) {
            CloseHandle(consoleInput);
            consoleInput = new IntPtr(-1);
        }
        FreeConsole();
    }

    public static void SendText(string text) {
        foreach (var character in text) {
            SendConsoleKey(0, character);
        }
    }

    public static void SendVirtualKey(ushort virtualKey) {
        var unicodeChar = virtualKey == 0x0D ? '\r' : '\0';
        SendConsoleKey(virtualKey, unicodeChar);
    }

    static void SendConsoleKey(ushort virtualKey, char unicodeChar) {
        var records = new[] {
            ConsoleKeyRecord(true, virtualKey, unicodeChar),
            ConsoleKeyRecord(false, virtualKey, unicodeChar),
        };
        if (!WriteConsoleInput(consoleInput, records, (uint) records.Length, out var written)) {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        if (written != records.Length) {
            throw new System.ComponentModel.Win32Exception("Incomplete console input write.");
        }
    }

    static CONSOLE_INPUT_RECORD ConsoleKeyRecord(
        bool keyDown,
        ushort virtualKey,
        char unicodeChar
    ) {
        return new CONSOLE_INPUT_RECORD {
            eventType = 1,
            keyEvent = new CONSOLE_KEY_EVENT {
                keyDown = keyDown,
                repeatCount = 1,
                virtualKey = virtualKey,
                unicodeChar = unicodeChar,
            },
        };
    }
}
'@

function Wait-ForPath {
    param(
        [string] $Path,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    while (-not (Test-Path -LiteralPath $Path)) {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Timed out waiting for '$Path'."
        }
        Start-Sleep -Milliseconds 100
    }
}

function Find-TerminalWindow {
    param(
        [string] $Title,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $nameCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        $Title
    )

    while ([DateTime]::UtcNow -lt $deadline) {
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition
        )

        foreach ($window in $windows) {
            try {
                if ($window.Current.ClassName -ne 'CASCADIA_HOSTING_WINDOW_CLASS') {
                    continue
                }

                $titleElement = $window.FindFirst(
                    [System.Windows.Automation.TreeScope]::Descendants,
                    $nameCondition
                )
                if ($null -ne $titleElement) {
                    return $window
                }
            } catch {
                # A window can disappear while its automation properties are being queried.
            }
        }

        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for Windows Terminal window '$Title'."
}

function Get-TerminalText {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal
    )

    $patternObject = $null
    if (-not $Terminal.TryGetCurrentPattern(
        [System.Windows.Automation.TextPattern]::Pattern,
        [ref] $patternObject
    )) {
        throw 'Windows Terminal TermControl does not expose TextPattern.'
    }

    $textPattern = [System.Windows.Automation.TextPattern] $patternObject
    return $textPattern.DocumentRange.GetText(-1)
}

function Get-TerminalControl {
    param(
        [string] $Title,
        [TimeSpan] $Timeout
    )

    $window = Find-TerminalWindow -Title $Title -Timeout $Timeout
    $classCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty,
        'TermControl'
    )
    $terminal = $window.FindFirst(
        [System.Windows.Automation.TreeScope]::Descendants,
        $classCondition
    )
    if ($null -eq $terminal) {
        throw 'Windows Terminal did not expose a TermControl automation element.'
    }
    return $terminal
}

function Connect-TerminalConsole {
    param(
        [string] $ProcessPath
    )

    $deadline = [DateTime]::UtcNow + [TimeSpan]::FromSeconds(30)
    while (-not (Test-Path -LiteralPath $ProcessPath)) {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Timed out waiting for console process '$ProcessPath'."
        }
        Start-Sleep -Milliseconds 100
    }

    $consoleProcessId = [uint32] [System.IO.File]::ReadAllText($ProcessPath)
    [CodexTerminalInput]::ConnectConsole($consoleProcessId)
}

function Wait-ForTerminalTextCount {
    param(
        [System.Windows.Automation.AutomationElement] $Terminal,
        [string] $Needle,
        [int] $Count,
        [TimeSpan] $Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    $lastText = ''
    while ([DateTime]::UtcNow -lt $deadline) {
        $lastText = Get-TerminalText -Terminal $Terminal
        $matchCount = [regex]::Matches($lastText, [regex]::Escape($Needle)).Count
        if ($matchCount -ge $Count) {
            return
        }
        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for $Count occurrences of '$Needle'. Last text:`n$lastText"
}

function Exit-Codex {
    [CodexTerminalInput]::SendText('/quit')
    Start-Sleep -Milliseconds 250
    [CodexTerminalInput]::SendVirtualKey(0x0D)
}

try {
    Connect-TerminalConsole -ProcessPath $ConsoleProcessPath
    try {
        $launchCommand = [System.IO.File]::ReadAllText($LaunchCommandPath)
        $terminal = Get-TerminalControl `
            -Title $WindowTitle `
            -Timeout ([TimeSpan]::FromSeconds(30))
        Wait-ForTerminalTextCount `
            -Terminal $terminal `
            -Needle $PromptMarker `
            -Count 1 `
            -Timeout ([TimeSpan]::FromSeconds(30))
        [CodexTerminalInput]::SendText($launchCommand)
        Wait-ForTerminalTextCount `
            -Terminal $terminal `
            -Needle 'danger-full-access' `
            -Count 1 `
            -Timeout ([TimeSpan]::FromSeconds(5))
        [CodexTerminalInput]::SendVirtualKey(0x0D)

    Wait-ForPath -Path $McpSignalPath -Timeout ([TimeSpan]::FromSeconds(60))
    Exit-Codex

    $terminal = Get-TerminalControl `
        -Title $WindowTitle `
        -Timeout ([TimeSpan]::FromSeconds(30))
    Wait-ForTerminalTextCount `
        -Terminal $terminal `
        -Needle $PromptMarker `
        -Count 2 `
        -Timeout ([TimeSpan]::FromSeconds(30))
    New-Item -ItemType File -Path $FirstExitPath | Out-Null
    New-Item -ItemType File -Path $SecondStartPath | Out-Null
    [CodexTerminalInput]::SendVirtualKey(0x26)
    Wait-ForTerminalTextCount `
        -Terminal $terminal `
        -Needle 'danger-full-access' `
        -Count 2 `
        -Timeout ([TimeSpan]::FromSeconds(5))
    [CodexTerminalInput]::SendVirtualKey(0x0D)
    Start-Sleep -Seconds 2
    New-Item -ItemType File -Path $PopupReadyPath | Out-Null

    Wait-ForPath -Path $PopupCapturedPath -Timeout ([TimeSpan]::FromSeconds(30))
    Exit-Codex
    $terminal = Get-TerminalControl `
        -Title $WindowTitle `
        -Timeout ([TimeSpan]::FromSeconds(30))
    Wait-ForTerminalTextCount `
        -Terminal $terminal `
        -Needle $PromptMarker `
        -Count 3 `
        -Timeout ([TimeSpan]::FromSeconds(30))
    [CodexTerminalInput]::SendText("Write-Output 'SECOND-EXIT-MARKER'")
    [CodexTerminalInput]::SendVirtualKey(0x0D)
    Wait-ForTerminalTextCount `
        -Terminal $terminal `
        -Needle 'SECOND-EXIT-MARKER' `
        -Count 1 `
        -Timeout ([TimeSpan]::FromSeconds(30))
        New-Item -ItemType File -Path $SecondExitPath | Out-Null
        New-Item -ItemType File -Path $InputCompletePath | Out-Null
    } finally {
        [CodexTerminalInput]::DisconnectConsole()
    }
    exit 0
} catch {
    [System.IO.File]::WriteAllText($FailurePath, $_.Exception.ToString())
    exit 1
}
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive Windows desktop and a locally built codex binary"]
async fn windows_terminal_preserves_scrollback_after_inline_viewport_height_change() -> Result<()> {
    run_windows_terminal_reflow_case(
        ReflowResponseScenario::FinalAnswer {
            sentinel_count: 100,
        },
        /*input_cycles*/ 1,
        /*simulate_mcp_startup*/ false,
    )
    .await?;
    run_windows_terminal_reflow_case(
        ReflowResponseScenario::FinalAnswer { sentinel_count: 5 },
        /*input_cycles*/ 2,
        /*simulate_mcp_startup*/ true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive Windows desktop and a locally built codex binary"]
async fn windows_terminal_tool_batches_do_not_insert_scrollback_gaps() -> Result<()> {
    run_windows_terminal_reflow_case(
        ReflowResponseScenario::ShellToolBatches {
            batch_count: 5,
            calls_per_batch: 4,
            sentinel_count: 20,
        },
        /*input_cycles*/ 1,
        /*simulate_mcp_startup*/ false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive Windows desktop and a locally built codex binary"]
async fn windows_terminal_clears_old_prompt_after_tool_preamble() -> Result<()> {
    run_windows_terminal_reflow_case(
        ReflowResponseScenario::PreambleThenShellTool { sentinel_count: 5 },
        /*input_cycles*/ 1,
        /*simulate_mcp_startup*/ true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive Windows desktop and a locally built codex binary"]
async fn windows_terminal_preserves_history_while_multiline_draft_is_open() -> Result<()> {
    run_windows_terminal_reflow_case(
        ReflowResponseScenario::LongComposer { sentinel_count: 18 },
        /*input_cycles*/ 1,
        /*simulate_mcp_startup*/ false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive Windows desktop and a locally built codex binary"]
async fn windows_terminal_preserves_wrapped_shell_rows_during_rapid_relaunch() -> Result<()> {
    let wt = which::which("wt.exe").context("Windows Terminal wt.exe is unavailable")?;
    let pwsh = which::which("pwsh.exe").context("PowerShell 7 pwsh.exe is unavailable")?;
    let codex = codex_utils_cargo_bin::cargo_bin("codex")
        .context("codex binary is unavailable; run `cargo build -p codex-cli` first")?;
    let artifacts = tempfile::Builder::new()
        .prefix("codex-windows-terminal-relaunch-")
        .tempdir()?
        .keep();
    let codex_home = artifacts.join("codex-home");
    let log_dir = artifacts.join("logs");
    let capture_dir = artifacts.join("captures");
    let workspace = artifacts.join("workspace");
    fs::create_dir_all(&codex_home)?;
    fs::create_dir_all(&log_dir)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(&workspace)?;

    eprintln!(
        "Windows Terminal relaunch artifacts: {}",
        artifacts.display()
    );

    let workspace_key = serde_json::to_string(&workspace.to_string_lossy())?;
    let pwsh_config_value = serde_json::to_string(&pwsh.to_string_lossy())?;
    fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model_provider = "openai"
suppress_unstable_features_warning = true

[projects.{workspace_key}]
trust_level = "trusted"

[mcp_servers.slow_startup]
command = {pwsh_config_value}
args = ["-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 5"]
startup_timeout_sec = 1
"#
        ),
    )?;
    fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;

    let window_title = artifacts
        .file_name()
        .and_then(|name| name.to_str())
        .context("Windows Terminal artifact directory has no file name")?
        .to_string();
    let launcher_path = artifacts.join("launch-codex.ps1");
    let driver_path = artifacts.join("drive-windows-terminal.ps1");
    let input_helper_path = artifacts.join("rapid-relaunch-input.ps1");
    let console_process_path = artifacts.join("console-process.txt");
    let launch_command_path = artifacts.join("launch-command.txt");
    let mcp_signal_path = artifacts.join("send-relaunch.signal");
    let popup_ready_path = artifacts.join("second-launch-ready.signal");
    let popup_captured_path = artifacts.join("second-launch-captured.signal");
    let input_complete_path = artifacts.join("rapid-relaunch.complete");
    let input_failure_path = artifacts.join("rapid-relaunch.failure.txt");
    let first_exit_path = artifacts.join("first-exit.signal");
    let second_start_path = artifacts.join("second-start.signal");
    let second_exit_path = artifacts.join("second-exit.signal");
    let log_dir_override = format!(
        "log_dir={}",
        serde_json::to_string(&log_dir.to_string_lossy())?
    );
    let prompt_marker = "CODEX-RELAUNCH-PROMPT>";
    fs::write(
        &launch_command_path,
        format!(
            "& {} --sandbox danger-full-access --ask-for-approval on-request --search \
             -c 'analytics.enabled=false' -c 'features.plugins=false' -c {} \
             -c 'tui.alternate_screen=\"never\"' -C {}",
            powershell_literal(&codex.to_string_lossy()),
            powershell_literal(&log_dir_override),
            powershell_literal(&workspace.to_string_lossy()),
        ),
    )?;
    fs::write(
        &launcher_path,
        format!(
            r#"$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = {window_title}
$env:CODEX_HOME = {codex_home}
$env:OPENAI_API_KEY = 'dummy'
$env:RUST_LOG = 'trace'
$null = [System.IO.File]::WriteAllText({console_process_path}, [string] $PID)
Start-Sleep -Seconds 2
Set-Location {workspace}
1..20 | ForEach-Object {{
    Write-Output ('PRE-RELAUNCH-SCROLLBACK-{{0:D3}}' -f $_)
}}
$helperArguments = @(
    '-NoLogo',
    '-NoProfile',
    '-File',
    {input_helper_path},
    '-WindowTitle',
    {window_title},
    '-ConsoleProcessPath',
    {console_process_path},
    '-LaunchCommandPath',
    {launch_command_path},
    '-PromptMarker',
    {prompt_marker},
    '-McpSignalPath',
    {mcp_signal_path},
    '-PopupReadyPath',
    {popup_ready_path},
    '-PopupCapturedPath',
    {popup_captured_path},
    '-InputCompletePath',
    {input_complete_path},
    '-FirstExitPath',
    {first_exit_path},
    '-SecondStartPath',
    {second_start_path},
    '-SecondExitPath',
    {second_exit_path},
    '-FailurePath',
    {input_failure_path}
)
Start-Process `
    -FilePath {pwsh} `
    -ArgumentList $helperArguments `
    -WindowStyle Hidden | Out-Null

Write-Output 'FIRST-LAUNCH-MARKER'
function global:prompt {{
    return {prompt_marker}
}}
"#,
            window_title = powershell_literal(&window_title),
            codex_home = powershell_literal(&codex_home.to_string_lossy()),
            workspace = powershell_literal(&workspace.to_string_lossy()),
            input_helper_path = powershell_literal(&input_helper_path.to_string_lossy()),
            console_process_path = powershell_literal(&console_process_path.to_string_lossy()),
            launch_command_path = powershell_literal(&launch_command_path.to_string_lossy()),
            prompt_marker = powershell_literal(prompt_marker),
            mcp_signal_path = powershell_literal(&mcp_signal_path.to_string_lossy()),
            popup_ready_path = powershell_literal(&popup_ready_path.to_string_lossy()),
            popup_captured_path = powershell_literal(&popup_captured_path.to_string_lossy()),
            input_complete_path = powershell_literal(&input_complete_path.to_string_lossy()),
            first_exit_path = powershell_literal(&first_exit_path.to_string_lossy()),
            second_start_path = powershell_literal(&second_start_path.to_string_lossy()),
            second_exit_path = powershell_literal(&second_exit_path.to_string_lossy()),
            input_failure_path = powershell_literal(&input_failure_path.to_string_lossy()),
            pwsh = powershell_literal(&pwsh.to_string_lossy()),
        ),
    )?;
    fs::write(&driver_path, WINDOWS_TERMINAL_DRIVER)?;
    fs::write(&input_helper_path, WINDOWS_TERMINAL_RELAUNCH_HELPER)?;

    let output = Command::new(&pwsh)
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-File")
        .arg(&driver_path)
        .arg("-WtPath")
        .arg(&wt)
        .arg("-PwshPath")
        .arg(&pwsh)
        .arg("-LauncherPath")
        .arg(&launcher_path)
        .arg("-WindowTitle")
        .arg(&window_title)
        .arg("-CaptureDirectory")
        .arg(&capture_dir)
        .arg("-ConsoleProcessPath")
        .arg(&console_process_path)
        .arg("-McpSignalPath")
        .arg(&mcp_signal_path)
        .arg("-PopupReadyPath")
        .arg(&popup_ready_path)
        .arg("-PopupCapturedPath")
        .arg(&popup_captured_path)
        .arg("-InputCompletePath")
        .arg(&input_complete_path)
        .arg("-FinalSentinel")
        .arg("OpenAI Codex")
        .arg("-Columns")
        .arg(TERMINAL_COLUMNS.to_string())
        .arg("-Rows")
        .arg(TERMINAL_ROWS.to_string())
        .output()
        .context("run Windows Terminal rapid-relaunch driver")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let input_failure = fs::read_to_string(&input_failure_path)
            .unwrap_or_else(|_| "input helper did not report a failure".to_string());
        bail!(
            "Windows Terminal rapid relaunch failed with {}.\nstdout:\n{stdout}\nstderr:\n{stderr}\n\
             input helper:\n{input_failure}\nartifacts: {}",
            output.status,
            artifacts.display()
        );
    }

    let first_launch = fs::read_to_string(capture_dir.join("baseline.txt"))?;
    let during_second_launch = fs::read_to_string(capture_dir.join("during-popup.txt"))?;
    assert_eq!(
        during_second_launch.matches("FIRST-LAUNCH-MARKER").count(),
        1,
        "expected the pre-launch marker to survive both launches; captures: {}",
        capture_dir.display()
    );
    assert_eq!(
        first_launch.matches("danger-full-access").count(),
        1,
        "expected the typed launch command to remain visible; captures: {}",
        capture_dir.display()
    );
    assert_eq!(
        during_second_launch.matches("danger-full-access").count(),
        2,
        "expected Up+Enter to replay the wrapped launch command exactly once; captures: {}",
        capture_dir.display()
    );
    let first_launch_header_count = first_launch.matches("OpenAI Codex").count();
    assert_eq!(
        first_launch_header_count,
        1,
        "expected exactly one header before relaunch; captures: {}",
        capture_dir.display()
    );
    assert_eq!(
        during_second_launch.matches("OpenAI Codex").count(),
        first_launch_header_count + 1,
        "expected the second launch to add exactly one header; captures: {}",
        capture_dir.display()
    );
    let initial_header_gap =
        blank_rows_between_launch_and_header(&first_launch, /*launch_index*/ 0)?;
    let settled_first_header_gap =
        blank_rows_between_launch_and_header(&during_second_launch, /*launch_index*/ 0)?;
    let settled_second_header_gap =
        blank_rows_between_launch_and_header(&during_second_launch, /*launch_index*/ 1)?;
    assert!(
        settled_first_header_gap <= MAX_SETTLED_LAUNCH_HEADER_GAP
            && settled_second_header_gap <= MAX_SETTLED_LAUNCH_HEADER_GAP,
        "settled launch-to-header gap exceeded the loading viewport: initial gap \
         {initial_header_gap}, first gap {settled_first_header_gap}, second gap \
         {settled_second_header_gap}; captures: {}",
        capture_dir.display()
    );
    assert!(
        settled_first_header_gap.abs_diff(settled_second_header_gap) <= 1,
        "rapid relaunch accumulated vertical drift: first settled header gap \
         {settled_first_header_gap}, second settled header gap {settled_second_header_gap}; \
         captures: {}",
        capture_dir.display()
    );
    assert_numbered_markers_are_contiguous(
        &during_second_launch,
        "PRE-RELAUNCH-SCROLLBACK",
        /*count*/ 20,
    )?;
    assert_session_headers_have_no_blank_interior(&during_second_launch)?;
    let settled_history_composer_gap =
        blank_rows_between_history_and_composer(&during_second_launch, SETTLED_MCP_HISTORY_MARKER)?;
    assert!(
        settled_history_composer_gap <= MAX_SETTLED_HISTORY_COMPOSER_GAP,
        "settled startup left {settled_history_composer_gap} blank rows between history and the \
         composer; captures: {}",
        capture_dir.display()
    );

    let after_second_exit = fs::read_to_string(capture_dir.join("after-mcp.txt"))?;
    assert!(
        after_second_exit.contains("SECOND-EXIT-MARKER"),
        "second Codex process did not return cleanly to PowerShell; captures: {}",
        capture_dir.display()
    );
    Ok(())
}

async fn run_windows_terminal_reflow_case(
    response_scenario: ReflowResponseScenario,
    input_cycles: usize,
    simulate_mcp_startup: bool,
) -> Result<()> {
    let wt = which::which("wt.exe").context("Windows Terminal wt.exe is unavailable")?;
    let pwsh = which::which("pwsh.exe").context("PowerShell 7 pwsh.exe is unavailable")?;
    let codex = codex_utils_cargo_bin::cargo_bin("codex")
        .context("codex binary is unavailable; run `cargo build -p codex-cli` first")?;
    let artifacts = tempfile::Builder::new()
        .prefix("codex-windows-terminal-reflow-")
        .tempdir()?
        .keep();
    let codex_home = artifacts.join("codex-home");
    let log_dir = artifacts.join("logs");
    let capture_dir = artifacts.join("captures");
    let workspace = artifacts.join("workspace");
    fs::create_dir_all(&codex_home)?;
    fs::create_dir_all(&log_dir)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(&workspace)?;

    eprintln!("Windows Terminal reflow artifacts: {}", artifacts.display());

    let server = MockServer::start().await;
    let response_mock = match response_scenario {
        ReflowResponseScenario::FinalAnswer { sentinel_count } => {
            responses::mount_response_once(
                &server,
                responses::sse_response(sentinel_response_sse(sentinel_count))
                    .set_delay(Duration::from_secs(8)),
            )
            .await
        }
        ReflowResponseScenario::PreambleThenShellTool { sentinel_count } => {
            responses::mount_sse_sequence(
                &server,
                preamble_then_shell_tool_sequence(sentinel_count),
            )
            .await
        }
        ReflowResponseScenario::LongComposer { sentinel_count } => {
            responses::mount_sse_sequence(&server, long_composer_response_sequence(sentinel_count))
                .await
        }
        ReflowResponseScenario::ShellToolBatches {
            batch_count,
            calls_per_batch,
            sentinel_count,
        } => {
            responses::mount_sse_sequence(
                &server,
                shell_tool_response_sequence(batch_count, calls_per_batch, sentinel_count),
            )
            .await
        }
    };
    let sentinel_count = response_scenario.sentinel_count();
    let model_provider_base_url = format!("{}/v1", server.uri());

    let workspace_key = serde_json::to_string(&workspace.to_string_lossy())?;
    let mcp_config = if simulate_mcp_startup {
        format!(
            r#"
[mcp_servers.slow_startup]
command = {}
args = ["-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 5"]
startup_timeout_sec = 1
"#,
            serde_json::to_string(&pwsh.to_string_lossy())?
        )
    } else {
        String::new()
    };
    fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model_provider = "mock"
suppress_unstable_features_warning = true

[model_providers.mock]
name = "mock"
base_url = {model_provider_base_url}
env_key = "OPENAI_API_KEY"
wire_api = "responses"
supports_websockets = false

[projects.{workspace_key}]
trust_level = "trusted"
{mcp_config}
"#,
            model_provider_base_url = serde_json::to_string(&model_provider_base_url)?,
        ),
    )?;
    fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;

    let window_title = artifacts
        .file_name()
        .and_then(|name| name.to_str())
        .context("Windows Terminal artifact directory has no file name")?
        .to_string();
    let launcher_path = artifacts.join("launch-codex.ps1");
    let driver_path = artifacts.join("drive-windows-terminal.ps1");
    let input_helper_path = artifacts.join("send-mcp-input.ps1");
    let console_process_path = artifacts.join("console-process.txt");
    let mcp_signal_path = artifacts.join("send-mcp.signal");
    let popup_ready_path = artifacts.join("popup-ready.signal");
    let popup_captured_path = artifacts.join("popup-captured.signal");
    let input_complete_path = artifacts.join("send-mcp.complete");
    let input_failure_path = artifacts.join("send-mcp.failure.txt");
    let log_dir_override = format!(
        "log_dir={}",
        serde_json::to_string(&log_dir.to_string_lossy())?
    );
    let working_prompt = format!(
        "{WORKING_PROMPT_START_MARKER}-{}-{WORKING_PROMPT_END_MARKER}",
        "X".repeat(usize::from(TERMINAL_COLUMNS) + 20),
    );
    let long_draft = matches!(
        response_scenario,
        ReflowResponseScenario::LongComposer { .. }
    )
    .then(|| {
        format!(
            "{LONG_DRAFT_START_MARKER}\n{}\n{}\n{}\n{LONG_DRAFT_END_MARKER}",
            "A".repeat(usize::from(TERMINAL_COLUMNS) + 20),
            "B".repeat(usize::from(TERMINAL_COLUMNS) + 20),
            "C".repeat(usize::from(TERMINAL_COLUMNS) + 20),
        )
    })
    .unwrap_or_default();
    let pending_input = matches!(
        response_scenario,
        ReflowResponseScenario::ShellToolBatches { .. }
    )
    .then(|| {
        format!(
            "{PENDING_INPUT_START_MARKER}-{}-{PENDING_INPUT_END_MARKER}",
            "Y".repeat(usize::from(TERMINAL_COLUMNS) + 20),
        )
    })
    .unwrap_or_default();
    let working_capture_needle = match response_scenario {
        ReflowResponseScenario::PreambleThenShellTool { .. } => Some(TOOL_AFTER_PREAMBLE_MARKER),
        ReflowResponseScenario::LongComposer { .. } => None,
        ReflowResponseScenario::FinalAnswer { .. }
        | ReflowResponseScenario::ShellToolBatches { .. } => Some(WORKING_PROMPT_END_MARKER),
    };
    fs::write(
        &launcher_path,
        format!(
            r#"$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = {window_title}
$env:CODEX_HOME = {codex_home}
$env:OPENAI_API_KEY = 'dummy'
$env:RUST_LOG = 'trace'
Start-Sleep -Seconds 2
Set-Location {workspace}
1..{preexisting_scrollback_count} | ForEach-Object {{
    Write-Output ('PRE-CODEX-SCROLLBACK-{{0:D3}}' -f $_)
}}
Write-Output ('{wrapped_launch_marker}-' + ('X' * ({terminal_columns} + 20)))
[System.IO.File]::WriteAllText({console_process_path}, [string] $PID)
$helperArguments = @(
    '-NoLogo',
    '-NoProfile',
    '-File',
    {input_helper_path},
    '-WindowTitle',
    {window_title},
    '-CaptureDirectory',
    {capture_dir},
    '-ConsoleProcessPath',
    {console_process_path},
    '-McpSignalPath',
    {mcp_signal_path},
    '-PopupReadyPath',
    {popup_ready_path},
    '-PopupCapturedPath',
    {popup_captured_path},
    '-InputCompletePath',
    {input_complete_path},
    '-InputCycles',
    '{input_cycles}',
    '-FailurePath',
    {input_failure_path}
)
Start-Process `
    -FilePath {pwsh} `
    -ArgumentList $helperArguments `
    -WindowStyle Hidden | Out-Null
& {codex} `
    -c 'analytics.enabled=false' `
    -c 'features.plugins=false' `
    -c {log_dir_override} `
    -c 'tui.alternate_screen="never"' `
    --sandbox danger-full-access `
    --ask-for-approval never `
    -C {workspace}
"#,
            window_title = powershell_literal(&window_title),
            codex_home = powershell_literal(&codex_home.to_string_lossy()),
            preexisting_scrollback_count = PREEXISTING_SCROLLBACK_COUNT,
            wrapped_launch_marker = WRAPPED_LAUNCH_MARKER,
            terminal_columns = TERMINAL_COLUMNS,
            workspace = powershell_literal(&workspace.to_string_lossy()),
            pwsh = powershell_literal(&pwsh.to_string_lossy()),
            input_helper_path = powershell_literal(&input_helper_path.to_string_lossy()),
            capture_dir = powershell_literal(&capture_dir.to_string_lossy()),
            console_process_path = powershell_literal(&console_process_path.to_string_lossy()),
            mcp_signal_path = powershell_literal(&mcp_signal_path.to_string_lossy()),
            popup_ready_path = powershell_literal(&popup_ready_path.to_string_lossy()),
            popup_captured_path = powershell_literal(&popup_captured_path.to_string_lossy()),
            input_complete_path = powershell_literal(&input_complete_path.to_string_lossy()),
            input_cycles = input_cycles,
            input_failure_path = powershell_literal(&input_failure_path.to_string_lossy()),
            codex = powershell_literal(&codex.to_string_lossy()),
            log_dir_override = powershell_literal(&log_dir_override),
        ),
    )?;
    fs::write(&driver_path, WINDOWS_TERMINAL_DRIVER)?;
    fs::write(&input_helper_path, WINDOWS_TERMINAL_INPUT_HELPER)?;

    let output = Command::new(&pwsh)
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-File")
        .arg(&driver_path)
        .arg("-WtPath")
        .arg(&wt)
        .arg("-PwshPath")
        .arg(&pwsh)
        .arg("-LauncherPath")
        .arg(&launcher_path)
        .arg("-WindowTitle")
        .arg(&window_title)
        .arg("-CaptureDirectory")
        .arg(&capture_dir)
        .arg("-ConsoleProcessPath")
        .arg(&console_process_path)
        .arg("-McpSignalPath")
        .arg(&mcp_signal_path)
        .arg("-PopupReadyPath")
        .arg(&popup_ready_path)
        .arg("-PopupCapturedPath")
        .arg(&popup_captured_path)
        .arg("-InputCompletePath")
        .arg(&input_complete_path)
        .arg("-FinalSentinel")
        .arg(format!("SENTINEL-{sentinel_count:03}"))
        .arg("-StartupCaptureNeedle")
        .arg("OpenAI Codex")
        .arg("-WorkingInput")
        .arg(&working_prompt)
        .arg("-WorkingCaptureNeedle")
        .arg(working_capture_needle.unwrap_or_default())
        .arg("-PendingInput")
        .arg(&pending_input)
        .arg("-PendingCaptureNeedle")
        .arg(if pending_input.is_empty() {
            ""
        } else {
            PENDING_STATUS_START_MARKER
        })
        .arg("-LongDraftInput")
        .arg(&long_draft)
        .arg("-LongDraftCaptureNeedle")
        .arg(if long_draft.is_empty() {
            ""
        } else {
            LONG_DRAFT_END_MARKER
        })
        .arg("-LongDraftAnchor")
        .arg(if long_draft.is_empty() {
            ""
        } else {
            LONG_HISTORY_ANCHOR_MARKER
        })
        .arg("-LongDraftSubmittedNeedle")
        .arg(if long_draft.is_empty() {
            ""
        } else {
            LONG_DRAFT_SUBMITTED_MARKER
        })
        .arg("-Columns")
        .arg(TERMINAL_COLUMNS.to_string())
        .arg("-Rows")
        .arg(TERMINAL_ROWS.to_string())
        .output()
        .context("run Windows Terminal automation driver")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let input_failure = fs::read_to_string(&input_failure_path)
            .unwrap_or_else(|_| "input helper did not report a failure".to_string());
        server.reset().await;
        bail!(
            "Windows Terminal automation failed with {}.\nstdout:\n{stdout}\nstderr:\n{stderr}\n\
             input helper:\n{input_failure}\nartifacts: {}",
            output.status,
            artifacts.display()
        );
    }

    assert_eq!(
        response_mock.requests().len(),
        response_scenario.expected_request_count(),
        "mock response request count"
    );

    let mut captures = vec![
        "startup".to_string(),
        "baseline".to_string(),
        "during-popup".to_string(),
        "after-mcp".to_string(),
        "after-maximize".to_string(),
        "after-restore".to_string(),
    ];
    if working_capture_needle.is_some() {
        captures.push("during-working".to_string());
    }
    if !pending_input.is_empty() {
        captures.push("during-pending-input".to_string());
    }
    if !long_draft.is_empty() {
        captures.extend(
            [
                "before-long-draft",
                "during-long-draft",
                "during-long-draft-scrolled",
                "after-long-draft-submit",
            ]
            .map(str::to_string),
        );
    }
    let mut input_captures = fs::read_dir(&capture_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| name.starts_with("input-step-"))
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    input_captures.sort();
    captures.extend(input_captures);

    let mut actual = BTreeMap::new();
    for capture in captures {
        let path = capture_dir.join(format!("{capture}.txt"));
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read Windows Terminal capture {}", path.display()))?;
        // The popup may cover the visible transcript tail, but it must not duplicate any row that
        // remains exposed above it.
        let may_cover_history = matches!(capture.as_str(), "during-popup" | "during-pending-input")
            || capture.starts_with("input-step-");
        let response_is_pending = matches!(
            capture.as_str(),
            "startup" | "during-working" | "during-pending-input"
        );
        let unexpected_response_counts = if may_cover_history || response_is_pending {
            duplicated_numbered_sentinel_counts(&text, "SENTINEL", sentinel_count)
        } else {
            unexpected_numbered_sentinel_counts(&text, "SENTINEL", sentinel_count)
        };
        actual.insert(format!("{capture}:response"), unexpected_response_counts);
        actual.insert(
            format!("{capture}:header"),
            if may_cover_history {
                duplicated_literal_count(&text, "OpenAI Codex")
            } else {
                unexpected_literal_count(&text, "OpenAI Codex")
            },
        );

        if matches!(
            capture.as_str(),
            "startup"
                | "during-working"
                | "during-pending-input"
                | "baseline"
                | "during-popup"
                | "after-mcp"
                | "before-long-draft"
                | "during-long-draft"
                | "during-long-draft-scrolled"
                | "after-long-draft-submit"
        ) || capture.starts_with("input-step-")
        {
            actual.insert(
                format!("{capture}:preexisting"),
                unexpected_numbered_sentinel_counts(
                    &text,
                    "PRE-CODEX-SCROLLBACK",
                    PREEXISTING_SCROLLBACK_COUNT,
                ),
            );
            actual.insert(
                format!("{capture}:wrapped-launch"),
                unexpected_literal_count(&text, WRAPPED_LAUNCH_MARKER),
            );
        }

        if !may_cover_history && capture != "startup" {
            let max_blank_rows = max_blank_row_run_after_session_header(&text)?;
            if max_blank_rows > MAX_TRANSCRIPT_BLANK_ROW_RUN {
                bail!(
                    "{capture} contains a {max_blank_rows}-row blank run inside transcript history; \
                     captures: {}",
                    capture_dir.display()
                );
            }
        }
    }
    if working_capture_needle.is_some() {
        let during_working = fs::read_to_string(capture_dir.join("during-working.txt"))?;
        let stray_prompt_rows = during_working
            .lines()
            .enumerate()
            .filter_map(|(index, line)| (line.trim() == "›").then_some(index + 1))
            .collect::<Vec<_>>();
        if !stray_prompt_rows.is_empty() {
            bail!(
                "during-working capture contains stray prompt glyphs on rows {stray_prompt_rows:?}; \
                 captures: {}",
                capture_dir.display()
            );
        }
        for marker in [WORKING_PROMPT_START_MARKER, WORKING_PROMPT_END_MARKER] {
            let count = during_working.matches(marker).count();
            if count != 1 {
                bail!(
                    "submitted prompt marker {marker:?} appeared {count} times while Working; \
                     captures: {}",
                    capture_dir.display()
                );
            }
        }
        if during_working.contains("SENTINEL-001") {
            bail!(
                "during-working capture was taken after assistant output began; captures: {}",
                capture_dir.display()
            );
        }
        if matches!(
            response_scenario,
            ReflowResponseScenario::FinalAnswer { .. }
                | ReflowResponseScenario::PreambleThenShellTool { .. }
        ) && !during_working.contains(WORKING_STATUS_MARKER)
        {
            bail!(
                "during-working capture clipped the Working status; captures: {}",
                capture_dir.display()
            );
        }
    }
    if !long_draft.is_empty() {
        let anchor_after_sentinel = sentinel_count / 2;
        for capture in [
            "before-long-draft",
            "during-long-draft",
            "during-long-draft-scrolled",
            "after-long-draft-submit",
        ] {
            let text = fs::read_to_string(capture_dir.join(format!("{capture}.txt")))?;
            let marker_positions = (1..=sentinel_count)
                .map(|index| {
                    let marker = format!("SENTINEL-{index:03}");
                    text.find(&marker)
                        .with_context(|| format!("{capture} is missing {marker}"))
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some((index, _)) = marker_positions
                .windows(2)
                .enumerate()
                .find(|(_, pair)| pair[0] >= pair[1])
            {
                bail!(
                    "{capture} reordered SENTINEL-{:03} and SENTINEL-{:03}; captures: {}",
                    index + 1,
                    index + 2,
                    capture_dir.display()
                );
            }

            let anchor_count = text.matches(LONG_HISTORY_ANCHOR_MARKER).count();
            let anchor_position = text
                .find(LONG_HISTORY_ANCHOR_MARKER)
                .with_context(|| format!("{capture} is missing {LONG_HISTORY_ANCHOR_MARKER}"))?;
            if anchor_count != 1
                || marker_positions[anchor_after_sentinel - 1] >= anchor_position
                || anchor_position >= marker_positions[anchor_after_sentinel]
            {
                bail!(
                    "{capture} did not preserve the numbered history around \
                     {LONG_HISTORY_ANCHOR_MARKER}; captures: {}",
                    capture_dir.display()
                );
            }

            let expected_draft_marker_count = usize::from(capture != "before-long-draft");
            for marker in [LONG_DRAFT_START_MARKER, LONG_DRAFT_END_MARKER] {
                let count = text.matches(marker).count();
                if count != expected_draft_marker_count {
                    bail!(
                        "{capture} contains {count} copies of {marker}, expected \
                         {expected_draft_marker_count}; captures: {}",
                        capture_dir.display()
                    );
                }
            }
            let stray_prompt_rows = text
                .lines()
                .enumerate()
                .filter_map(|(index, line)| (line.trim() == "›").then_some(index + 1))
                .collect::<Vec<_>>();
            if !stray_prompt_rows.is_empty() {
                bail!(
                    "{capture} contains stray prompt glyphs on rows {stray_prompt_rows:?}; \
                     captures: {}",
                    capture_dir.display()
                );
            }
        }

        let after_submit = fs::read_to_string(capture_dir.join("after-long-draft-submit.txt"))?;
        let submitted_count = after_submit.matches(LONG_DRAFT_SUBMITTED_MARKER).count();
        if submitted_count != 1 {
            bail!(
                "after-long-draft-submit contains {submitted_count} copies of \
                 {LONG_DRAFT_SUBMITTED_MARKER}; captures: {}",
                capture_dir.display()
            );
        }
    }
    if !pending_input.is_empty() {
        let during_pending = fs::read_to_string(capture_dir.join("during-pending-input.txt"))?;
        for marker in [
            WORKING_PROMPT_START_MARKER,
            WORKING_PROMPT_END_MARKER,
            PENDING_INPUT_START_MARKER,
            PENDING_INPUT_END_MARKER,
            PENDING_STATUS_START_MARKER,
            PENDING_STATUS_END_MARKER,
        ] {
            let count = during_pending.matches(marker).count();
            if count != 1 {
                bail!(
                    "pending-input marker {marker:?} appeared {count} times; captures: {}",
                    capture_dir.display()
                );
            }
        }
    }
    if simulate_mcp_startup {
        let baseline = fs::read_to_string(capture_dir.join("baseline.txt"))?;
        if !baseline.contains("MCP startup") {
            bail!(
                "simulated MCP startup did not reach the captured terminal; captures: {}",
                capture_dir.display()
            );
        }
    }
    let expected = actual
        .keys()
        .map(|capture| (capture.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        actual,
        expected,
        "Windows Terminal dropped or duplicated response or pre-existing scrollback rows; \
         captures: {}",
        capture_dir.display()
    );
    Ok(())
}

fn sentinel_response_sse(sentinel_count: usize) -> String {
    let text = (1..=sentinel_count)
        .map(|index| format!("- SENTINEL-{index:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    responses::sse(vec![
        responses::ev_response_created("resp-windows-terminal-reflow"),
        responses::ev_assistant_message("msg-windows-terminal-reflow", &text),
        responses::ev_completed("resp-windows-terminal-reflow"),
    ])
}

fn long_composer_response_sequence(sentinel_count: usize) -> Vec<String> {
    let mut history = (1..=sentinel_count)
        .map(|index| format!("- SENTINEL-{index:03}"))
        .collect::<Vec<_>>();
    history.insert(sentinel_count / 2, LONG_HISTORY_ANCHOR_MARKER.to_string());
    let history = history.join("\n");

    vec![
        responses::sse(vec![
            responses::ev_response_created("resp-long-composer-history"),
            responses::ev_assistant_message("msg-long-composer-history", &history),
            responses::ev_completed("resp-long-composer-history"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("resp-long-composer-submitted"),
            responses::ev_assistant_message(
                "msg-long-composer-submitted",
                LONG_DRAFT_SUBMITTED_MARKER,
            ),
            responses::ev_completed("resp-long-composer-submitted"),
        ]),
    ]
}

fn preamble_then_shell_tool_sequence(sentinel_count: usize) -> Vec<String> {
    let preamble = format!(
        "{TOOL_PREAMBLE_MARKER}-{}",
        "P".repeat(usize::from(TERMINAL_COLUMNS) + 20)
    );
    let exec_source = format!(
        r#"const result = await tools.shell_command({{
  command: "Write-Output '{TOOL_AFTER_PREAMBLE_MARKER}'; Start-Sleep -Seconds 4",
  timeout_ms: 10000
}});
text(result);"#
    );
    vec![
        responses::sse(vec![
            responses::ev_response_created("resp-tool-preamble"),
            responses::ev_assistant_message("msg-tool-preamble", &preamble),
            responses::ev_custom_tool_call("exec-tool-preamble", "exec", &exec_source),
            responses::ev_completed("resp-tool-preamble"),
        ]),
        sentinel_response_sse(sentinel_count),
    ]
}

fn shell_tool_response_sequence(
    batch_count: usize,
    calls_per_batch: usize,
    sentinel_count: usize,
) -> Vec<String> {
    let mut sequence = Vec::with_capacity(batch_count + 1);
    for batch in 1..=batch_count {
        let response_id = format!("resp-tool-batch-{batch}");
        let mut commands = Vec::with_capacity(calls_per_batch);
        for call in 1..=calls_per_batch {
            let marker = format!("TOOL-BATCH-{batch:03}-CALL-{call:03}");
            commands.push(format!(
                "1..8 | ForEach-Object {{ Write-Output ('{marker}-ROW-{{0:D2}}-' -f $_ + ('X' * 140)); Start-Sleep -Milliseconds {TOOL_ROW_DELAY_MS} }}"
            ));
        }
        let commands = serde_json::to_string(&commands).expect("serialize shell commands");
        let exec_source = format!(
            r#"const commands = {commands};
const results = await Promise.allSettled(commands.map(command => tools.shell_command({{ command, timeout_ms: 10000 }})));
for (const result of results) {{
  text(result.status === "fulfilled" ? result.value : String(result.reason));
}}"#
        );
        sequence.push(responses::sse(vec![
            responses::ev_response_created(&response_id),
            responses::ev_custom_tool_call(
                &format!("exec-tool-batch-{batch}"),
                "exec",
                &exec_source,
            ),
            responses::ev_completed(&response_id),
        ]));
    }
    sequence.push(sentinel_response_sse(sentinel_count));
    sequence
}

fn unexpected_numbered_sentinel_counts(text: &str, prefix: &str, count: usize) -> Vec<String> {
    (1..=count)
        .map(|index| format!("{prefix}-{index:03}"))
        .filter_map(|sentinel| {
            let actual_count = text.matches(&sentinel).count();
            (actual_count != 1).then_some(format!("{sentinel}={actual_count}"))
        })
        .collect()
}

fn duplicated_numbered_sentinel_counts(text: &str, prefix: &str, count: usize) -> Vec<String> {
    (1..=count)
        .map(|index| format!("{prefix}-{index:03}"))
        .filter_map(|sentinel| {
            let actual_count = text.matches(&sentinel).count();
            (actual_count > 1).then_some(format!("{sentinel}={actual_count}"))
        })
        .collect()
}

fn unexpected_literal_count(text: &str, literal: &str) -> Vec<String> {
    let actual_count = text.matches(literal).count();
    (actual_count != 1)
        .then_some(format!("{literal}={actual_count}"))
        .into_iter()
        .collect()
}

fn duplicated_literal_count(text: &str, literal: &str) -> Vec<String> {
    let actual_count = text.matches(literal).count();
    (actual_count > 1)
        .then_some(format!("{literal}={actual_count}"))
        .into_iter()
        .collect()
}

fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
