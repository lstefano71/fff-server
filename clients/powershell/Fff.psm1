<#
.SYNOPSIS
    PowerShell client for fff-server.

.DESCRIPTION
    Thin wrapper that returns PowerShell objects, so results pipe into Where-Object,
    Select-Object, Out-GridView, or anything else. Nothing here formats results into text
    and hopes you parse it back - that is the whole reason fff-server exists.

    Quick start:

        Import-Module ./Fff.psm1
        Connect-FffServer -BaseUrl http://s81alfa01:8080 -Root '\\server\share\project'
        ff 'src/**/*.cs controller'          # find files
        fg 'TODO' | Format-FffMatch          # search content, with match highlighting
#>

Set-StrictMode -Version Latest

$script:Fff = @{
    BaseUrl     = $null
    WorkspaceId = $null
    Root        = $null
    Token       = $null
}

# ---------------------------------------------------------------------------- internals

function Get-FffAnsi {
    # $PSStyle exists on PowerShell 7.2+. Fall back to raw escapes, and to nothing at all
    # when output is redirected, so piping to a file does not collect escape codes.
    param([switch]$Force)
    if (-not $Force -and -not $Host.UI.SupportsVirtualTerminal) { return $null }
    $esc = [char]27
    return @{
        Match   = "$esc[1;33m"   # bold yellow
        Path    = "$esc[36m"     # cyan
        Line    = "$esc[90m"     # grey
        Def     = "$esc[1;35m"   # bold magenta
        Reset   = "$esc[0m"
    }
}

function Invoke-FffApi {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        $Body,
        [int]$TimeoutSeconds = 300
    )

    if (-not $script:Fff.BaseUrl) {
        throw 'Not connected. Run Connect-FffServer first.'
    }

    $uri = "$($script:Fff.BaseUrl.TrimEnd('/'))$Path"
    $args = @{
        Method      = $Method
        Uri         = $uri
        TimeoutSec  = $TimeoutSeconds
        ErrorAction = 'Stop'
    }
    if ($script:Fff.Token) {
        $args.Headers = @{ Authorization = "Bearer $($script:Fff.Token)" }
    }
    if ($null -ne $Body) {
        # Depth matters: structured queries and cache budgets nest.
        $args.Body = ($Body | ConvertTo-Json -Depth 10 -Compress)
        $args.ContentType = 'application/json; charset=utf-8'
    }

    try {
        return Invoke-RestMethod @args
    }
    catch {
        # The server speaks RFC 9457 problem+json, so surface the machine-readable code and
        # the detail rather than PowerShell's generic HTTP message.
        $detail = $null
        if ($_.ErrorDetails -and $_.ErrorDetails.Message) {
            try { $detail = $_.ErrorDetails.Message | ConvertFrom-Json } catch { }
        }
        if ($detail -and $detail.PSObject.Properties.Name -contains 'code') {
            throw "fff-server [$($detail.code)] $($detail.detail)"
        }
        throw
    }
}

# ---------------------------------------------------------------------------- connection

function Connect-FffServer {
    <#
    .SYNOPSIS
        Point at a server and index a root, waiting for it to become searchable.

    .DESCRIPTION
        Creation is idempotent: an already-indexed root is returned rather than rebuilt, and
        any spelling of the same directory resolves to the same workspace.

        A large share takes a while - an 87k-file tree measured 59s to scan and longer to
        finish its content index - so the server returns 202 and this polls, reporting
        progress. Content indexing is what fuzzy grep needs, so -WaitFor Indexing is the
        default.

    .EXAMPLE
        Connect-FffServer -BaseUrl http://s81alfa01:8080 -Root '\\server\share\project'

    .EXAMPLE
        # Just get searchable as fast as possible; fuzzy grep may be incomplete for a while.
        Connect-FffServer -BaseUrl http://localhost:8080 -Root D:\devel\fff -WaitFor Scan
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$BaseUrl,
        [Parameter(Mandatory)][string]$Root,
        [ValidateSet('Scan', 'Indexing', 'Watcher')][string]$WaitFor = 'Indexing',
        [string]$Token,
        [int]$TimeoutSeconds = 900,
        [switch]$Quiet
    )

    $script:Fff.BaseUrl = $BaseUrl
    $script:Fff.Token = $Token

    $health = Invoke-FffApi -Method GET -Path '/v1/health' -TimeoutSeconds 30
    if (-not $Quiet) {
        Write-Host "Connected to fff-server $($health.version) (engine $($health.engineVersion)) at $BaseUrl"
    }

    $waitFor = $WaitFor.ToLowerInvariant()
    $ws = Invoke-FffApi -Method POST -Path '/v1/workspaces' -TimeoutSeconds 120 -Body @{
        root           = $Root
        waitFor        = $waitFor
        waitForIndexMs = 10000
    }

    $script:Fff.WorkspaceId = $ws.id
    $script:Fff.Root = $ws.root

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $started = Get-Date
    while (-not (Test-FffWorkspaceReadiness -Workspace $ws -WaitFor $waitFor) -and
        (Get-Date) -lt $deadline) {
        if (-not $Quiet) {
            $elapsed = [int]((Get-Date) - $started).TotalSeconds
            $progressStatus = Get-FffWorkspaceProgressStatus -Workspace $ws `
                -ElapsedSeconds $elapsed
            Write-Progress -Activity "Preparing $($ws.root)" `
                -Status $progressStatus `
                -PercentComplete -1
        }
        Start-Sleep -Milliseconds 700
        $ws = Invoke-FffApi -Method GET -Path "/v1/workspaces/$($ws.id)" -TimeoutSeconds 30
    }
    if (-not $Quiet) { Write-Progress -Activity 'Indexing' -Completed }

    if (-not (Test-FffWorkspaceReadiness -Workspace $ws -WaitFor $waitFor) -and -not $Quiet) {
        Write-Warning "Workspace did not reach the requested '$waitFor' readiness stage after ${TimeoutSeconds}s. Current status is '$($ws.status)'."
    }

    if (-not $Quiet) {
        $secs = [math]::Round($ws.timeToReadyMs / 1000, 1)
        Write-Host "Workspace $($ws.id): $($ws.indexedFiles) files, ready in ${secs}s" -NoNewline
        if ($ws.isNetworkPath) { Write-Host ' (network path)' -NoNewline }
        Write-Host ''
        Write-Host "Auto-rescan every $($ws.rescanIntervalSecs)s; idle eviction after $($ws.idleTimeoutSecs)s" -ForegroundColor DarkGray
    }

    $ws
}

function Get-FffWorkspace {
    <# .SYNOPSIS Current workspace status, including readiness and derived intervals. #>
    [CmdletBinding()]
    param([string]$Id = $script:Fff.WorkspaceId)
    if (-not $Id) { throw 'No workspace. Run Connect-FffServer first.' }
    Invoke-FffApi -Method GET -Path "/v1/workspaces/$Id"
}

function Get-FffHealth {
    <# .SYNOPSIS Server liveness and live workspace count. #>
    [CmdletBinding()] param()
    Invoke-FffApi -Method GET -Path '/v1/health'
}

function Assert-FffWorkspace {
    if (-not $script:Fff.WorkspaceId) { throw 'No workspace. Run Connect-FffServer first.' }
    $script:Fff.WorkspaceId
}

function Get-FffPropertyValue {
    param(
        [Parameter(Mandatory)]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )

    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    $property.Value
}

function Test-FffWorkspaceReadiness {
    param(
        [Parameter(Mandatory)]$Workspace,
        [Parameter(Mandatory)]
        [ValidateSet('scan', 'indexing', 'watcher')]
        [string]$WaitFor
    )

    switch ($WaitFor) {
        'scan' { return $Workspace.status -ne 'initialising' }
        'indexing' { return [bool]$Workspace.isWarmupComplete }
        'watcher' { return [bool]$Workspace.isWatcherReady }
    }
}

function Get-FffWorkspaceProgressStatus {
    param(
        [Parameter(Mandatory)]$Workspace,
        [Parameter(Mandatory)][int]$ElapsedSeconds
    )

    if ($Workspace.status -eq 'initialising') {
        return "Scanning: $($Workspace.scannedFilesCount) files discovered (${ElapsedSeconds}s)"
    }
    if (-not $Workspace.isWarmupComplete) {
        return "Building content index: $($Workspace.indexedFiles) files searchable (${ElapsedSeconds}s)"
    }
    if (-not $Workspace.isWatcherReady) {
        return "Starting filesystem watcher: $($Workspace.indexedFiles) files searchable (${ElapsedSeconds}s)"
    }
    "Ready: $($Workspace.indexedFiles) files searchable (${ElapsedSeconds}s)"
}

# ---------------------------------------------------------------------------- searching

function Find-FffFile {
    <#
    .SYNOPSIS
        Fuzzy file search. Returns objects.

    .DESCRIPTION
        The query is fff's DSL, passed through untouched: constraint tokens plus fuzzy terms.
        Glob separators must be forward slashes.

            git:modified src/**/*.cs !tests/ user controller

        Typo-resistant, so 'shcema' finds 'schema'.

    .EXAMPLE
        ff 'controller'

    .EXAMPLE
        # Objects, so ordinary PowerShell works on them.
        ff '*.cs' -Take 500 | Where-Object Size -gt 100kb | Sort-Object Size -Descending

    .EXAMPLE
        ff 'git:modified' | Out-GridView
    #>
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Query = '',
        [int]$Take = 30,
        [int]$Page = 0,
        # Deprioritises this path and boosts nearby ones.
        [string]$CurrentFile,
        # Keep paging until the server runs out.
        [switch]$All
    )

    $id = Assert-FffWorkspace
    $page = $Page
    do {
        $body = @{ query = $Query; page = $page; pageSize = $Take }
        if ($CurrentFile) { $body.currentFile = $CurrentFile }
        $result = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/search" -Body $body

        foreach ($w in @(Get-FffPropertyValue -InputObject $result -Name 'warnings')) {
            if ($w) { Write-Warning $w }
        }

        foreach ($hit in @($result.items)) {
            [pscustomobject]@{
                PSTypeName   = 'Fff.FileHit'
                Path         = $hit.item.relativePath
                Name         = $hit.item.fileName
                Score        = $hit.score.total
                Match        = $hit.score.matchType
                Git          = $hit.item.gitStatus.status
                GitFlags     = $hit.item.gitStatus.flags
                Size         = $hit.item.size
                Modified     = [datetime]$hit.item.modified
                Frecency     = $hit.item.totalFrecencyScore
                GitRecency   = $hit.item.gitRecencyScore
                FullPath     = $hit.item.absolutePath
                Ranges       = $hit.matchRangesUtf16
                Breakdown    = $hit.score
            }
        }
        $page++
    } while ($All -and $result.hasMore)
}

function Find-FffDirectory {
    <# .SYNOPSIS Fuzzy directory search. #>
    [CmdletBinding()]
    param([Parameter(Position = 0)][string]$Query = '', [int]$Take = 30)

    $id = Assert-FffWorkspace
    $result = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/search/directories" `
        -Body @{ query = $Query; pageSize = $Take }

    foreach ($hit in @($result.items)) {
        [pscustomobject]@{
            PSTypeName = 'Fff.DirHit'
            Path       = $hit.item.relativePath
            Name       = $hit.item.dirName
            Score      = $hit.score.total
            Frecency   = $hit.item.maxAccessFrecency
            FullPath   = $hit.item.absolutePath
        }
    }
}

function Find-FffGlob {
    <#
    .SYNOPSIS
        Literal glob match, frecency-ranked, bypassing the fuzzy parser.

    .DESCRIPTION
        Use this when the pattern is already a glob and fuzzy matching on top would only add
        noise. Backslashes are folded to forward slashes for you here, because a glob field
        is unambiguous.

    .EXAMPLE
        Find-FffGlob '**/*.aplf' -Take 200
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0)][string]$Pattern, [int]$Take = 100, [switch]$All)

    $id = Assert-FffWorkspace
    $pattern = $Pattern.Replace('\', '/')
    $page = 0
    do {
        $result = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/glob" `
            -Body @{ pattern = $pattern; page = $page; pageSize = $Take }
        foreach ($hit in @($result.items)) {
            [pscustomobject]@{
                PSTypeName = 'Fff.FileHit'
                Path       = $hit.item.relativePath
                Name       = $hit.item.fileName
                Score      = $hit.score.total
                Size       = $hit.item.size
                Modified   = [datetime]$hit.item.modified
                Git        = $hit.item.gitStatus.status
                FullPath   = $hit.item.absolutePath
            }
        }
        $page++
    } while ($All -and $result.hasMore)
}

function Find-FffText {
    <#
    .SYNOPSIS
        Content search. Returns one object per matching line.

    .DESCRIPTION
        Modes: Auto (default) picks regex when the pattern contains metacharacters, otherwise
        literal. Fuzzy is typo-tolerant but slower, and needs the content index finished.

        Constraint tokens work here too: '*.cs !tests/ TODO'.

        The Ranges property holds UTF-16 offsets into Text, which index .NET strings
        directly - that is what Format-FffMatch uses to highlight.

        Grep over a network share measured about 7ms per file, so a full sweep of a very
        large tree takes minutes. -All pages through everything; without it you get the
        first page.

    .EXAMPLE
        fg 'TODO' | Format-FffMatch

    .EXAMPLE
        fg 'IsDateTimePicker' -Context 2 -All | Format-FffMatch

    .EXAMPLE
        fg 'catch\s*\(' -Mode Regex | Group-Object Path | Sort-Object Count -Descending
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Query,
        [ValidateSet('Auto', 'Plain', 'Regex', 'Fuzzy')][string]$Mode = 'Auto',
        [ValidateSet('Smart', 'Sensitive', 'Insensitive')][string]$Casing = 'Smart',
        [int]$Take = 50,
        [int]$Context = 0,
        # Tag lines that look like definitions.
        [switch]$Definitions,
        [switch]$All,
        # Stop after this many milliseconds and return what was found. 0 = unbounded.
        [int]$TimeBudgetMs = 0
    )

    $id = Assert-FffWorkspace
    $cursor = $null
    $pages = 0

    do {
        $body = @{
            query               = $Query
            mode                = $Mode.ToLowerInvariant()
            casing              = $Casing.ToLowerInvariant()
            pageSize            = $Take
            beforeContext       = $Context
            afterContext        = $Context
            classifyDefinitions = [bool]$Definitions
            timeBudgetMs        = $TimeBudgetMs
        }
        if ($cursor) { $body.cursor = $cursor }

        $result = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/grep" -Body $body
        $pages++

        $regexFallbackError = Get-FffPropertyValue -InputObject $result -Name 'regexFallbackError'
        if ($regexFallbackError) {
            Write-Warning "Regex did not compile, fell back to literal matching: $regexFallbackError"
        }
        if ($result.literalFallback) {
            Write-Warning 'Constrained query found nothing; results come from retrying the whole query as literal text.'
        }

        foreach ($m in @($result.matches)) {
            $file = $result.files[$m.fileIndex]
            [pscustomobject]@{
                PSTypeName = 'Fff.TextHit'
                Path       = $file.relativePath
                Line       = $m.lineNumber
                Col        = $m.colUtf16
                Text       = $m.lineContent
                IsDef      = $m.isDefinition
                Ranges     = $m.matchRangesUtf16
                Before     = $m.contextBefore
                After      = $m.contextAfter
                Git        = $file.gitStatus.status
                FullPath   = $file.absolutePath
                ByteOffset = $m.byteOffset
            }
        }

        $cursor = Get-FffPropertyValue -InputObject $result -Name 'nextCursor'
    } while ($All -and $cursor -and $pages -lt 1000)
}

function Format-FffMatch {
    <#
    .SYNOPSIS
        Renders text hits with the matched span highlighted.

    .DESCRIPTION
        Uses the UTF-16 ranges the server reports. Because .NET strings are UTF-16 those
        offsets index Text directly, with no conversion - which is why they are reported in
        those units. Highlighting is therefore correct on lines containing APL glyphs, em
        dashes, emoji, or anything else non-ASCII.

    .EXAMPLE
        fg 'TODO' -Context 1 | Format-FffMatch
    #>
    [CmdletBinding()]
    param(
        [Parameter(ValueFromPipeline)]$Hit,
        [switch]$NoColour
    )

    begin {
        $ansi = if ($NoColour) { $null } else { Get-FffAnsi }
        $lastPath = $null
    }

    process {
        if (-not $Hit) { return }

        if ($Hit.Path -ne $lastPath) {
            if ($lastPath) { Write-Host '' }
            if ($ansi) { Write-Host "$($ansi.Path)$($Hit.Path)$($ansi.Reset)" }
            else { Write-Host $Hit.Path }
            $lastPath = $Hit.Path
        }

        foreach ($c in @($Hit.Before)) {
            if ($ansi) { Write-Host "$($ansi.Line)      | $c$($ansi.Reset)" } else { Write-Host "      | $c" }
        }

        # Rebuild the line with the matched spans wrapped. Ranges are half-open and in
        # ascending order; walking them keeps the untouched text intact.
        $text = [string]$Hit.Text
        $out = ''
        $pos = 0
        foreach ($r in @($Hit.Ranges)) {
            $start = [int]$r.start
            $end = [int]$r.end
            if ($start -lt $pos -or $end -gt $text.Length -or $end -le $start) { continue }
            $out += $text.Substring($pos, $start - $pos)
            $span = $text.Substring($start, $end - $start)
            $out += if ($ansi) { "$($ansi.Match)$span$($ansi.Reset)" } else { "[$span]" }
            $pos = $end
        }
        $out += $text.Substring($pos)

        $marker = if ($Hit.IsDef) { if ($ansi) { "$($ansi.Def)def$($ansi.Reset)" } else { 'def' } } else { '   ' }
        $num = '{0,5}' -f $Hit.Line
        if ($ansi) { Write-Host "$($ansi.Line)$num$($ansi.Reset) $marker $out" }
        else { Write-Host "$num $marker $out" }

        foreach ($c in @($Hit.After)) {
            if ($ansi) { Write-Host "$($ansi.Line)      | $c$($ansi.Reset)" } else { Write-Host "      | $c" }
        }
    }
}

# ---------------------------------------------------------------------------- actions

function Open-FffFile {
    <#
    .SYNOPSIS
        Opens a hit and tells the server it was opened, so frecency ranks it higher next time.

    .DESCRIPTION
        This is the half of frecency a server cannot observe for itself. Reporting opens is
        what makes results improve with use.

    .EXAMPLE
        ff 'controller' | Select-Object -First 1 | Open-FffFile

    .EXAMPLE
        fg 'TODO' | Select-Object -First 1 | Open-FffFile -Editor code
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$Hit,
        # Anything on PATH. VS Code understands file:line.
        [string]$Editor,
        [switch]$NoTrack
    )

    process {
        $id = Assert-FffWorkspace

        if (-not $NoTrack) {
            try {
                $tracked = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/track-access" `
                    -Body @{ path = $Hit.Path }
                Write-Verbose "Tracked $($tracked.relativePath), opened $($tracked.accessCount) time(s)"
            }
            catch {
                Write-Warning "Could not record the access: $_"
            }
        }

        if ($Editor) {
            $line = if ($Hit.PSObject.Properties.Name -contains 'Line') { $Hit.Line } else { $null }
            if ($Editor -eq 'code' -and $line) { & $Editor --goto "$($Hit.FullPath):$line" }
            else { & $Editor $Hit.FullPath }
        }
        else {
            Invoke-Item -LiteralPath $Hit.FullPath
        }
    }
}

function Update-FffIndex {
    <#
    .SYNOPSIS
        Force a rescan, or refresh git status.

    .DESCRIPTION
        The automatic rescan interval is derived from how long indexing actually took, and
        reaches 30 minutes on a very large share - so tell the server when you know something
        changed rather than waiting. -Git refreshes cached git status instead, which is what
        you want after a commit or a branch switch.
    #>
    [CmdletBinding()]
    param([switch]$Git)

    $id = Assert-FffWorkspace
    if ($Git) {
        $r = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/git/refresh" -Body @{}
        Write-Host "Refreshed git status: $($r.updated) file(s) changed"
    }
    else {
        $ws = Invoke-FffApi -Method POST -Path "/v1/workspaces/$id/rescan" -Body @{}
        Write-Host "Rescan started; workspace is '$($ws.status)'"
        $ws
    }
}

function Test-FffQuery {
    <#
    .SYNOPSIS
        Shows how the server parses a query, without searching.

    .DESCRIPTION
        The DSL is quiet about mistakes: a token that looks like a glob but is not recognised
        as one becomes fuzzy text instead, and the results still look plausible. This shows
        what the server actually understood, and warns about the common traps.

    .EXAMPLE
        Test-FffQuery 'git:modified src/**/*.cs !tests/ user controller'

    .EXAMPLE
        # The classic Windows mistake: backslashes in a glob.
        Test-FffQuery 'src\**\*.cs'
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Query,
        [ValidateSet('FileSearch', 'DirSearch', 'MixedSearch', 'Grep', 'AiGrep')]
        [string]$Preset = 'FileSearch'
    )

    $preset = $Preset.Substring(0, 1).ToLowerInvariant() + $Preset.Substring(1)
    $parsed = Invoke-FffApi -Method POST -Path '/v1/parse-query' `
        -Body @{ query = $Query; preset = $preset }

    foreach ($w in @($parsed.warnings)) { if ($w) { Write-Warning $w } }
    $parsed
}

Set-Alias -Name ff -Value Find-FffFile
Set-Alias -Name fg -Value Find-FffText
Set-Alias -Name fd -Value Find-FffDirectory

Export-ModuleMember -Function Connect-FffServer, Get-FffWorkspace, Get-FffHealth,
Find-FffFile, Find-FffDirectory, Find-FffGlob, Find-FffText, Format-FffMatch,
Open-FffFile, Update-FffIndex, Test-FffQuery -Alias ff, fg, fd
