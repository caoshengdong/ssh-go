<#
.SYNOPSIS
Runs end-to-end sgo tests on Windows using a disposable Docker OpenSSH server.

.DESCRIPTION
The script builds sgo, starts a temporary Alpine-based SSH server container,
then verifies:
  - key-based interactive login path
  - password-based interactive login path via PTY password entry
  - key-based sgo exec
  - password-based sgo exec
  - local tunnel traffic (-L)
  - dynamic SOCKS tunnel traffic (-D)
  - reverse tunnel traffic (-R)

All sgo config, SSH keys, logs, and known_hosts files are placed under a
temporary directory and removed by default.
#>

#requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$NoBuild,
    [switch]$KeepArtifacts,
    [switch]$KeepContainer
)

$ErrorActionPreference = "Stop"

$ProjectRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$SgoPath = Join-Path $ProjectRoot "target\debug\sgo.exe"
$RunId = [guid]::NewGuid().ToString("N").Substring(0, 12)
$WorkDir = Join-Path $env:TEMP "sgo-e2e-$RunId"
$DockerImage = "alpine:3.20"
$DockerImageToRemove = $null
$DockerContainer = "sgo-e2e-$RunId"
$Password = "sgo-password-123"
$RemoteHttpPort = 8081

$originalEnv = @{
    SGO_CONFIG_DIR = $env:SGO_CONFIG_DIR
    SGO_NO_TTY    = $env:SGO_NO_TTY
    HOME           = $env:HOME
    USERPROFILE    = $env:USERPROFILE
}

$startedProcesses = New-Object System.Collections.ArrayList
$reverseJob = $null

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content
    )

    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, ($Content -replace "`r`n", "`n"), $encoding)
}

function Get-FreeTcpPort {
    $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try {
        return ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    }
    finally {
        $listener.Stop()
    }
}

function Wait-TcpPort {
    param(
        [string]$HostName = "127.0.0.1",
        [Parameter(Mandatory = $true)][int]$Port,
        [int]$TimeoutSeconds = 30
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $client = New-Object System.Net.Sockets.TcpClient
        try {
            $async = $client.BeginConnect($HostName, $Port, $null, $null)
            if ($async.AsyncWaitHandle.WaitOne(500)) {
                $client.EndConnect($async)
                return
            }
        }
        catch {
            Start-Sleep -Milliseconds 200
        }
        finally {
            $client.Close()
        }
    }

    throw "Timed out waiting for TCP $HostName`:$Port"
}

function Invoke-External {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @(),
        [int[]]$ExpectedExitCodes = @(0),
        [int]$TimeoutSeconds = 300
    )

    $commandId = [guid]::NewGuid().ToString("N")
    $stdoutPath = Join-Path $WorkDir "command-$commandId.stdout.log"
    $stderrPath = Join-Path $WorkDir "command-$commandId.stderr.log"

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $FilePath
    $psi.Arguments = ConvertTo-ProcessArguments $Arguments
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $psi
    [void]$process.Start()

    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $timedOut = $true
        try {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            [void]$process.WaitForExit(5000)
        }
        catch {}
    } else {
        $timedOut = $false
    }

    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    Write-Utf8NoBom -Path $stdoutPath -Content $stdout
    Write-Utf8NoBom -Path $stderrPath -Content $stderr
    $output = "$stdout$stderr"
    $exitCode = $process.ExitCode

    if ($timedOut) {
        throw "Command timed out after $TimeoutSeconds seconds: $FilePath $($Arguments -join ' ')`n$output"
    }

    if ($ExpectedExitCodes -notcontains $exitCode) {
        throw "Command failed ($exitCode): $FilePath $($Arguments -join ' ')`n$output"
    }
    return $output
}

function ConvertTo-ProcessArguments {
    param([string[]]$Arguments = @())

    ($Arguments | ForEach-Object { ConvertTo-ProcessArgument $_ }) -join " "
}

function ConvertTo-ProcessArgument {
    param([AllowEmptyString()][string]$Argument)

    if ($Argument -notmatch '[\s"]' -and $Argument.Length -gt 0) {
        return $Argument
    }

    $result = '"'
    $backslashes = 0
    foreach ($char in $Argument.ToCharArray()) {
        if ($char -eq '\') {
            $backslashes++
            continue
        }

        if ($char -eq '"') {
            $result += ('\' * (($backslashes * 2) + 1))
            $result += '"'
            $backslashes = 0
            continue
        }

        if ($backslashes -gt 0) {
            $result += ('\' * $backslashes)
            $backslashes = 0
        }
        $result += $char
    }

    if ($backslashes -gt 0) {
        $result += ('\' * ($backslashes * 2))
    }

    $result += '"'
    return $result
}

function Invoke-Sgo {
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int[]]$ExpectedExitCodes = @(0),
        [int]$TimeoutSeconds = 300
    )

    Invoke-External -FilePath $SgoPath -Arguments $Arguments -ExpectedExitCodes $ExpectedExitCodes -TimeoutSeconds $TimeoutSeconds
}

function Assert-Contains {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if (-not $Text.Contains($Expected)) {
        throw "$Context did not contain '$Expected'. Actual output:`n$Text"
    }
}

function Start-SgoBackground {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $stdout = Join-Path $WorkDir "$Name.stdout.log"
    $stderr = Join-Path $WorkDir "$Name.stderr.log"
    $process = Start-Process -FilePath $SgoPath `
        -ArgumentList $Arguments `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr `
        -PassThru
    [void]$startedProcesses.Add($process)
    return [pscustomobject]@{ Process = $process; Stdout = $stdout; Stderr = $stderr }
}

function Stop-TrackedProcess {
    param([Parameter(Mandatory = $true)]$Tracked)

    if ($Tracked.Process -and -not $Tracked.Process.HasExited) {
        Stop-Process -Id $Tracked.Process.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $Tracked.Process.Id -ErrorAction SilentlyContinue
    }
}

function Invoke-SgoLoginPath {
    param(
        [Parameter(Mandatory = $true)][string]$Alias
    )

    $lastError = $null
    $deadline = (Get-Date).AddSeconds(20)
    do {
        try {
            return Invoke-Sgo -Arguments @($Alias) -TimeoutSeconds 20
        }
        catch {
            $lastError = $_
            if ((Get-Date) -ge $deadline) {
                throw
            }
            Start-Sleep -Milliseconds 500
        }
    } while ($true)
}

function Start-ReverseHttpServer {
    param([Parameter(Mandatory = $true)][int]$Port)

    Start-Job -ArgumentList $Port -ScriptBlock {
        param([int]$Port)
        $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, $Port)
        $listener.Start()
        try {
            while ($true) {
                $client = $listener.AcceptTcpClient()
                try {
                    $stream = $client.GetStream()
                    $buffer = New-Object byte[] 1024
                    if ($stream.DataAvailable) {
                        [void]$stream.Read($buffer, 0, $buffer.Length)
                    }
                    $body = "SGO_REVERSE_OK`n"
                    $response = "HTTP/1.1 200 OK`r`nContent-Type: text/plain`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n$body"
                    $bytes = [System.Text.Encoding]::ASCII.GetBytes($response)
                    $stream.Write($bytes, 0, $bytes.Length)
                }
                finally {
                    $client.Close()
                }
            }
        }
        finally {
            $listener.Stop()
        }
    }
}

function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Action
    )

    Write-Host "==> $Name"
    & $Action
    Write-Host "PASS: $Name" -ForegroundColor Green
}

function Cleanup {
    foreach ($process in $startedProcesses) {
        if ($process -and -not $process.HasExited) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }

    if ($reverseJob) {
        Stop-Job $reverseJob -ErrorAction SilentlyContinue
        Remove-Job $reverseJob -Force -ErrorAction SilentlyContinue
    }

    if (-not $KeepContainer) {
        try {
            & docker rm -f $DockerContainer *> $null
            if ($DockerImageToRemove) {
                & docker rmi -f $DockerImageToRemove *> $null
            }
        }
        catch {
            Write-Verbose "Docker cleanup skipped: $($_.Exception.Message)"
        }
    }

    $env:SGO_CONFIG_DIR = $originalEnv.SGO_CONFIG_DIR
    $env:SGO_NO_TTY = $originalEnv.SGO_NO_TTY
    $env:HOME = $originalEnv.HOME
    $env:USERPROFILE = $originalEnv.USERPROFILE

    if (-not $KeepArtifacts -and (Test-Path $WorkDir)) {
        Remove-Item -Recurse -Force $WorkDir
    }
    elseif (Test-Path $WorkDir) {
        Write-Host "Artifacts kept at: $WorkDir"
    }
}

try {
    New-Item -ItemType Directory -Path $WorkDir | Out-Null
    $ConfigDir = Join-Path $WorkDir "config"
    New-Item -ItemType Directory -Path $ConfigDir | Out-Null

    $script:SshPort = $null
    $LocalForwardPort = Get-FreeTcpPort
    $DynamicPort = Get-FreeTcpPort
    $ReverseRemotePort = Get-FreeTcpPort
    $LocalHttpPort = Get-FreeTcpPort

    Invoke-Step "check prerequisites" {
        Invoke-External -FilePath "docker" -Arguments @("--version") | Write-Host
        Invoke-External -FilePath "ssh" -Arguments @("-V") | Write-Host
        Invoke-External -FilePath "ssh-keygen" -Arguments @("-V") -ExpectedExitCodes @(0, 1) | Out-Null
    }

    Invoke-Step "build sgo" {
        if (-not $NoBuild) {
            Push-Location $ProjectRoot
            try {
                Invoke-External -FilePath "cargo" -Arguments @("build") | Write-Host
            }
            finally {
                Pop-Location
            }
        }

        if (-not (Test-Path $SgoPath)) {
            throw "sgo executable not found at $SgoPath"
        }
    }

    $KeyPath = Join-Path $WorkDir "id_ed25519"
    $PublicKeyPath = "$KeyPath.pub"
    $SetupScriptPath = Join-Path $WorkDir "setup-sshd.sh"
    Invoke-Step "generate SSH key" {
        Invoke-External -FilePath "ssh-keygen" -Arguments @("-t", "ed25519", "-N", "", "-f", $KeyPath, "-q") | Out-Null
    }

    Invoke-Step "prepare disposable SSH server setup" {
        Write-Utf8NoBom -Path $SetupScriptPath -Content @"
#!/bin/sh
set -eu
apk add --no-cache openssh-server python3 curl
adduser -D -s /bin/sh execer
adduser -D -s /bin/sh login
echo 'execer:$Password' | chpasswd
echo 'login:$Password' | chpasswd
mkdir -p /run/sshd /home/execer/.ssh /tmp/ssh-content
mkdir -p /home/login/.ssh
cp /tmp/authorized_keys /home/execer/.ssh/authorized_keys
cp /tmp/authorized_keys /home/login/.ssh/authorized_keys
chmod 700 /home/execer/.ssh
chmod 700 /home/login/.ssh
chown -R execer:execer /home/execer/.ssh
chown -R login:login /home/login/.ssh
chmod 600 /home/execer/.ssh/authorized_keys
chmod 600 /home/login/.ssh/authorized_keys
ssh-keygen -A
printf 'SGO_TUNNEL_OK\n' > /tmp/ssh-content/index.html
cat >> /etc/ssh/sshd_config <<'EOF'
PasswordAuthentication yes
PubkeyAuthentication yes
PermitRootLogin no
AllowUsers execer login
AllowTcpForwarding yes
GatewayPorts yes
PermitOpen any

Match User login
    ForceCommand /bin/sh -c "echo SGO_LOGIN_OK"
EOF
(cd /tmp/ssh-content && python3 -m http.server 8081 --bind 127.0.0.1 >/tmp/http.log 2>&1 &)
exec /usr/sbin/sshd -D -e
"@
    }

    Invoke-Step "start disposable SSH server" {
        Invoke-External -FilePath "docker" -Arguments @(
            "run", "-d",
            "--name", $DockerContainer,
            "-p", "127.0.0.1::22",
            "-v", "${SetupScriptPath}:/setup.sh:ro",
            "-v", "${PublicKeyPath}:/tmp/authorized_keys:ro",
            $DockerImage,
            "sh", "/setup.sh"
        ) | Write-Host
        $published = Invoke-External -FilePath "docker" -Arguments @("port", $DockerContainer, "22/tcp")
        $script:SshPort = [int]($published.Trim().Split(":")[-1])
        Wait-TcpPort -Port $SshPort -TimeoutSeconds 120
        Start-Sleep -Seconds 2
    }

    Invoke-Step "write temporary sgo config" {
        # Keep OpenSSH known_hosts and sgo config isolated from the real Windows profile.
        # This is intentionally done after cargo/ssh-keygen/docker setup because rustup
        # may use USERPROFILE to find the configured default toolchain on Windows.
        $env:SGO_CONFIG_DIR = $ConfigDir
        $env:SGO_NO_TTY = "1"
        $env:HOME = $WorkDir
        $env:USERPROFILE = $WorkDir

        $serversJson = @"
[
  {"alias":"login-key","host":"127.0.0.1","port":$SshPort,"user":"login","auth":{"type":"key","value":"$($KeyPath -replace '\\', '\\')"}},
  {"alias":"login-pass","host":"127.0.0.1","port":$SshPort,"user":"login","auth":{"type":"password","value":"$Password"}},
  {"alias":"keybox","host":"127.0.0.1","port":$SshPort,"user":"execer","auth":{"type":"key","value":"$($KeyPath -replace '\\', '\\')"}},
  {"alias":"passbox","host":"127.0.0.1","port":$SshPort,"user":"execer","auth":{"type":"password","value":"$Password"}}
]
"@
        Write-Utf8NoBom -Path (Join-Path $ConfigDir "servers.json") -Content $serversJson
        $list = Invoke-Sgo -Arguments @("list")
        Assert-Contains -Text $list -Expected "login-key" -Context "sgo list"
        Assert-Contains -Text $list -Expected "login-pass" -Context "sgo list"
        Assert-Contains -Text $list -Expected "keybox" -Context "sgo list"
        Assert-Contains -Text $list -Expected "passbox" -Context "sgo list"
    }

    Invoke-Step "key-based login path" {
        $output = Invoke-SgoLoginPath -Alias "login-key"
        Assert-Contains -Text $output -Expected "SGO_LOGIN_OK" -Context "key login"
    }

    Invoke-Step "password-based login path via PTY password entry" {
        $output = Invoke-SgoLoginPath -Alias "login-pass"
        Assert-Contains -Text $output -Expected "Connecting to login@127.0.0.1" -Context "password login"
    }

    Invoke-Step "key-based exec" {
        $output = Invoke-Sgo -Arguments @("exec", "keybox", 'echo${IFS}SGO_EXEC_KEY_OK')
        Assert-Contains -Text $output -Expected "SGO_EXEC_KEY_OK" -Context "key exec"
    }

    Invoke-Step "password-based exec via PTY password entry" {
        $output = Invoke-Sgo -Arguments @("exec", "passbox", 'echo${IFS}SGO_EXEC_PASS_OK')
        Assert-Contains -Text $output -Expected "SGO_EXEC_PASS_OK" -Context "password exec"
    }

    Invoke-Step "local tunnel traffic (-L)" {
        $tunnel = Start-SgoBackground -Name "local-tunnel" -Arguments @("tunnel", "keybox", "$LocalForwardPort`:127.0.0.1:$RemoteHttpPort")
        try {
            Wait-TcpPort -Port $LocalForwardPort -TimeoutSeconds 20
            $body = Invoke-External -FilePath "curl.exe" -Arguments @("--silent", "--show-error", "--fail", "--max-time", "10", "http://127.0.0.1:$LocalForwardPort/")
            Assert-Contains -Text $body -Expected "SGO_TUNNEL_OK" -Context "local tunnel response"
        }
        finally {
            Stop-TrackedProcess -Tracked $tunnel
            if ($reverseJob) {
                Stop-Job $reverseJob -ErrorAction SilentlyContinue
                Remove-Job $reverseJob -Force -ErrorAction SilentlyContinue
                $reverseJob = $null
            }
        }
    }

    Invoke-Step "dynamic SOCKS tunnel traffic (-D)" {
        $tunnel = Start-SgoBackground -Name "dynamic-tunnel" -Arguments @("tunnel", "keybox", "-d", "$DynamicPort")
        try {
            Wait-TcpPort -Port $DynamicPort -TimeoutSeconds 20
            $body = Invoke-External -FilePath "curl.exe" -Arguments @("--silent", "--show-error", "--fail", "--max-time", "10", "--socks5-hostname", "127.0.0.1:$DynamicPort", "http://127.0.0.1:$RemoteHttpPort/")
            Assert-Contains -Text $body -Expected "SGO_TUNNEL_OK" -Context "dynamic tunnel response"
        }
        finally {
            Stop-TrackedProcess -Tracked $tunnel
        }
    }

    Invoke-Step "reverse tunnel traffic (-R)" {
        $reverseJob = Start-ReverseHttpServer -Port $LocalHttpPort
        Wait-TcpPort -Port $LocalHttpPort -TimeoutSeconds 10

        $tunnel = Start-SgoBackground -Name "reverse-tunnel" -Arguments @("tunnel", "keybox", "-r", "$ReverseRemotePort`:$LocalHttpPort")
        try {
            $body = $null
            $deadline = (Get-Date).AddSeconds(20)
            do {
                try {
                    $body = Invoke-External -FilePath "docker" -Arguments @("exec", $DockerContainer, "curl", "--silent", "--show-error", "--fail", "--max-time", "5", "http://127.0.0.1:$ReverseRemotePort/")
                    break
                }
                catch {
                    if ((Get-Date) -ge $deadline) {
                        throw
                    }
                    Start-Sleep -Milliseconds 500
                }
            } while ($true)

            Assert-Contains -Text $body -Expected "SGO_REVERSE_OK" -Context "reverse tunnel response"
        }
        finally {
            Stop-TrackedProcess -Tracked $tunnel
        }
    }

    Write-Host "All end-to-end SSH tests passed." -ForegroundColor Green
}
finally {
    Cleanup
}












