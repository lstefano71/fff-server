$ErrorActionPreference = 'Stop'

function Assert-Equal {
    param(
        [Parameter(Mandatory)]$Expected,
        [Parameter(Mandatory)]$Actual,
        [Parameter(Mandatory)][string]$Message
    )

    if ($Expected -ne $Actual) {
        throw "$Message Expected '$Expected', got '$Actual'."
    }
}

$module = Import-Module "$PSScriptRoot\Fff.psm1" -Force -PassThru

try {
    & $module {
        $script:Fff.BaseUrl = 'http://test'
        $script:Fff.WorkspaceId = 'workspace'
        $script:CapturedGrepBody = $null

        function Invoke-FffApi {
            param($Method, $Path, $Body, $TimeoutSeconds)

            if ($Path -like '*/glob') {
                $script:CapturedPattern = $Body.pattern
                return [pscustomobject]@{ items = @(); hasMore = $false }
            }
        }

        Find-FffGlob 'src\**\*.cs' | Out-Null
        Assert-Equal 'src/**/*.cs' $script:CapturedPattern 'Find-FffGlob must normalize separators.'
    }

    & $module {
        $script:Fff.BaseUrl = 'http://test'
        $script:Fff.WorkspaceId = 'workspace'

        function Invoke-FffApi {
            param($Method, $Path, $Body, $TimeoutSeconds)

            if ($Path -like '*/search') {
                return [pscustomobject]@{ items = @(); hasMore = $false }
            }
            if ($Path -like '*/grep') {
                $script:CapturedGrepBody = $Body
                return [pscustomobject]@{
                    matches         = @()
                    files           = @()
                    literalFallback = $false
                }
            }
        }

        Find-FffFile 'missing optional fields' | Out-Null
        Find-FffText 'missing optional fields' -TimeBudgetMs 100 -EnforceTimeBudget | Out-Null
        Assert-Equal $true $script:CapturedGrepBody.enforceTimeBudget `
            'Find-FffText must expose the zero-match time-budget safeguard.'
    }

    & $module {
        $workspace = [pscustomobject]@{
            status             = 'indexing'
            scannedFilesCount  = 86936
            indexedFiles       = 86936
            isWarmupComplete   = $false
            isWatcherReady     = $false
        }

        $status = Get-FffWorkspaceProgressStatus -Workspace $workspace -ElapsedSeconds 349
        Assert-Equal 'Building content index: 86936 files searchable (349s)' $status `
            'The progress message must distinguish searchable files from content-index completion.'
    }

    & $module {
        $script:Polls = 0

        function New-TestWorkspace {
            param(
                [string]$Status,
                [bool]$WarmupComplete,
                [bool]$WatcherReady
            )

            [pscustomobject]@{
                id                 = 'workspace'
                root               = 'C:\repo'
                status             = $Status
                isWarmupComplete   = $WarmupComplete
                isWatcherReady     = $WatcherReady
                scannedFilesCount  = 1
                indexedFiles       = 1
                timeToReadyMs      = 1
                isNetworkPath      = $false
                rescanIntervalSecs = 60
                idleTimeoutSecs    = 600
            }
        }

        function Invoke-FffApi {
            param($Method, $Path, $Body, $TimeoutSeconds)

            if ($Path -eq '/v1/health') {
                return [pscustomobject]@{ version = 'test'; engineVersion = 'test' }
            }
            if ($Method -eq 'POST') {
                return New-TestWorkspace 'indexing' $false $false
            }

            $script:Polls++
            return New-TestWorkspace 'ready' $true $true
        }

        Connect-FffServer -BaseUrl http://test -Root C:\repo -WaitFor Scan -Quiet | Out-Null
        Assert-Equal 0 $script:Polls 'WaitFor Scan must return once files are searchable.'
    }

    & $module {
        $script:Polls = 0

        function New-TestWorkspace {
            param([bool]$WatcherReady)

            [pscustomobject]@{
                id                 = 'workspace'
                root               = 'C:\repo'
                status             = 'ready'
                isWarmupComplete   = $true
                isWatcherReady     = $WatcherReady
                scannedFilesCount  = 1
                indexedFiles       = 1
                timeToReadyMs      = 1
                isNetworkPath      = $false
                rescanIntervalSecs = 60
                idleTimeoutSecs    = 600
            }
        }

        function Invoke-FffApi {
            param($Method, $Path, $Body, $TimeoutSeconds)

            if ($Path -eq '/v1/health') {
                return [pscustomobject]@{ version = 'test'; engineVersion = 'test' }
            }
            if ($Method -eq 'POST') {
                return New-TestWorkspace $false
            }

            $script:Polls++
            return New-TestWorkspace $true
        }

        $workspace = Connect-FffServer -BaseUrl http://test -Root C:\repo `
            -WaitFor Watcher -Quiet
        Assert-Equal 1 $script:Polls 'WaitFor Watcher must poll until live updates are ready.'
        Assert-Equal $true $workspace.isWatcherReady 'WaitFor Watcher returned too early.'
    }

    'PowerShell client tests passed.'
}
finally {
    Remove-Module $module.Name -ErrorAction SilentlyContinue
}
