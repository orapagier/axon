<#
.SYNOPSIS
    Prepares a laptop to stay reachable for remote automation.

.DESCRIPTION
    The service handles being locked. It cannot do anything about being asleep --
    a sleeping laptop has no network, so the Cloudflare tunnel drops and every
    route fails regardless of how the API is architected. On a laptop this is
    the failure you will actually hit, and it is pure configuration.

    Changes made (all scoped to AC power, so battery behaviour is untouched):
      - never sleep or hibernate while plugged in
      - closing the lid while plugged in does nothing
      - keep the network up during connected standby
      - display still sleeps, which is fine and saves the panel
      - stop Windows powering down the network adapter
      - put the wireless radio in maximum-performance mode on AC

    The adapter power settings matter more than they sound. A Wi-Fi card that
    Windows is allowed to park does not announce it: the link stays "connected",
    and what you see instead is outbound TCP connections timing out for tens of
    seconds at a time and long-lived ones dying. That is indistinguishable from
    a bad ISP, and it is the single most common cause of a tunnel that flaps on
    a laptop while the same machine browses the web perfectly well.

    Optionally enables Remote Desktop as a break-glass path. Worth having: when
    the automation layer wedges, you need a way in that does not depend on the
    automation layer. It is opt-in because it widens the machine's attack
    surface, and should only ever be reachable over Tailscale, a Cloudflare
    tunnel, or a VPN -- never a forwarded port on your router.

.PARAMETER EnableRdp
    Also enable the Remote Desktop host and its firewall rules.

.PARAMETER Revert
    Restore Windows' default AC power behaviour (sleep after 30 min, lid sleeps).

.EXAMPLE
    .\prepare-laptop.ps1
    .\prepare-laptop.ps1 -EnableRdp
    .\prepare-laptop.ps1 -Revert
#>

#requires -RunAsAdministrator

# KEEP THIS FILE PURE ASCII.
#
# It ships without a byte-order mark, and Windows PowerShell 5.1 reads a
# BOM-less file as ANSI rather than UTF-8. A UTF-8 em-dash then decodes to
# "a<0x80><0x94>", and 0x94 in CP1252 is a right smart-quote, which the parser
# honours as a string delimiter. One em-dash inside a double-quoted string is
# therefore enough to terminate that string early and silently swallow every
# function defined after it -- which is exactly what used to happen to
# Show-Summary here: the script ran, made its changes, then died on its last
# line claiming the function did not exist.
#
# A BOM would also fix it, but ASCII survives being copied, patched and
# re-encoded by an installer, and a BOM does not.

[CmdletBinding()]
param(
    [switch]$EnableRdp,
    [switch]$Revert
)

$ErrorActionPreference = 'Stop'

# Power setting GUIDs. powercfg accepts aliases for some of these but not all,
# so the raw GUIDs keep it consistent.
$SUB_BUTTONS        = '4f971e89-eebd-4455-a8de-9e59040e7347'
$LID_ACTION         = '5ca83367-6e45-459f-a27b-476b1d01c936'
$SUB_SLEEP          = '238c9fa8-0aad-41ed-83f4-97be242c8f20'
$CONNECTIVITY_IN_S0 = 'f15576e8-98b7-4186-b944-eafa664402d9'
# Wireless Adapter Settings > Power Saving Mode. 0 = Maximum Performance.
$SUB_WIRELESS       = '19cbb8fa-5279-450e-9fac-8a3d5fedd0c1'
$WIFI_POWER_MODE    = '12bbebe6-58d6-4636-95bb-3217ef867c1a'

<#
    Runs powercfg, returns its stdout lines, and never throws.

    Native stderr needs care in Windows PowerShell 5.1: redirecting it inside
    the shell wraps every line in an ErrorRecord, and under
    $ErrorActionPreference = 'Stop' that ErrorRecord is terminating. So a
    machine that simply lacks an optional subgroup -- connected standby does not
    exist on plenty of hardware -- would abort the entire script on a setting it
    was never going to need. Dropping to 'Continue' for the call and filtering
    the records back out keeps 'Stop' everywhere it is actually wanted.

    Note this is why plain `2>$null` is not the fix here: the redirection is
    what creates the ErrorRecord in the first place.
#>
function Invoke-Powercfg {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & powercfg.exe @args 2>&1 |
            Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] }
    }
    finally {
        $ErrorActionPreference = $saved
        # powercfg's exit code is not the script's, and an optional setting
        # being absent must not leak out as a failed run.
        $global:LASTEXITCODE = 0
    }
}

function Assert-Admin {
    $identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Error "Run this from an elevated PowerShell (Run as Administrator)."
    }
}

function Set-PowerBehaviour {
    if ($Revert) {
        Write-Host "Restoring default AC power behaviour..." -ForegroundColor Cyan
        Invoke-Powercfg /change standby-timeout-ac 30
        Invoke-Powercfg /change hibernate-timeout-ac 180
        # 1 = Sleep
        Invoke-Powercfg /setacvalueindex SCHEME_CURRENT $SUB_BUTTONS $LID_ACTION 1
    }
    else {
        Write-Host "Keeping the machine awake on AC..." -ForegroundColor Cyan
        # 0 = never
        Invoke-Powercfg /change standby-timeout-ac 0
        Invoke-Powercfg /change hibernate-timeout-ac 0
        # The display can still sleep -- it does not affect the tunnel, and the
        # desktop agent keeps capturing because the session stays active.
        Invoke-Powercfg /change monitor-timeout-ac 15
        # 0 = Do nothing on lid close
        Invoke-Powercfg /setacvalueindex SCHEME_CURRENT $SUB_BUTTONS $LID_ACTION 0
        # 1 = network stays connected in modern standby. Absent on machines
        # without S0 standby, which is fine and not worth reporting.
        Invoke-Powercfg /setacvalueindex SCHEME_CURRENT $SUB_SLEEP $CONNECTIVITY_IN_S0 1
        # 0 = Maximum Performance. The default (Medium/Maximum Power Saving)
        # lets the radio doze between beacons, which shows up as multi-second
        # stalls on established connections rather than as a dropped link.
        Invoke-Powercfg /setacvalueindex SCHEME_CURRENT $SUB_WIRELESS $WIFI_POWER_MODE 0
    }

    Invoke-Powercfg /setactive SCHEME_CURRENT
    Write-Host "  done." -ForegroundColor Green
}

<#
    Stops Windows powering down the network adapters themselves.

    This is separate from the power *scheme* above: it lives in Device Manager
    on the adapter's Power Management tab, survives scheme changes, and is
    enabled by default on nearly every laptop. While it is on, the OS may cut
    power to the NIC during idle and the first packets after a wake are simply
    lost -- which the application above sees as a dial timeout to an edge server
    that is, in fact, perfectly reachable.

    Wired adapters get the same treatment: a laptop docked over Ethernet has the
    identical failure mode.
#>
function Set-AdapterPowerBehaviour {
    if ($Revert) {
        Write-Host "Restoring default adapter power management..." -ForegroundColor Cyan
    }
    else {
        Write-Host "Stopping Windows powering down the network adapters..." -ForegroundColor Cyan
    }

    $adapters = Get-NetAdapter -Physical -ErrorAction SilentlyContinue |
                Where-Object { $_.Status -eq 'Up' }

    if (-not $adapters) {
        Write-Host "  no active physical adapters found; skipped." -ForegroundColor Yellow
        return
    }

    $want = if ($Revert) { 'Enabled' } else { 'Disabled' }

    foreach ($a in $adapters) {
        # Set-NetAdapterPowerManagement has NO -AllowComputerToTurnOffDevice
        # parameter, despite Get- returning it as a property. The supported way
        # is to fetch the object, mutate the property, and hand the whole thing
        # back via -InputObject. Passing it as a switch fails with "a parameter
        # cannot be found", which reads exactly like an unsupported driver and
        # is why this went unnoticed.
        try {
            $pm = Get-NetAdapterPowerManagement -Name $a.Name -ErrorAction Stop
            $pm.AllowComputerToTurnOffDevice = $want
            Set-NetAdapterPowerManagement -InputObject $pm -ErrorAction Stop
            Write-Host ("  {0,-10} device power-down: {1}" -f $a.Name, $want.ToLower()) `
                -ForegroundColor Green
        }
        catch {
            # Genuinely unsupported by some virtual and older drivers. Report
            # the reason rather than guessing at one.
            Write-Host ("  {0,-10} device power-down unchanged: {1}" -f $a.Name, $_.Exception.Message) `
                -ForegroundColor DarkGray
        }

        # Intel wireless drivers park spatial streams separately from anything
        # Windows' own power settings control. The values are 0 Auto SMPS,
        # 1 Static, 2 Dynamic, 3 No SMPS -- so "off" is 3, not 0. Setting 0 here
        # asks for the aggressive default under the impression it is disabling
        # it, which is the sort of mistake that looks like it worked.
        $mimo = if ($Revert) { 0 } else { 3 }
        try {
            Set-NetAdapterAdvancedProperty -Name $a.Name -RegistryKeyword 'MIMOPowerSaveMode' `
                -RegistryValue $mimo -NoRestart -ErrorAction Stop
            $now = (Get-NetAdapterAdvancedProperty -Name $a.Name `
                    -RegistryKeyword 'MIMOPowerSaveMode' -ErrorAction Stop).DisplayValue
            Write-Host ("  {0,-10} MIMO power save: {1}" -f $a.Name, $now) -ForegroundColor Green
        }
        catch {
            # Only Intel wireless exposes this one, so absence is the norm.
        }
    }

    Write-Host "  note: adapter changes take effect on the next link reset." -ForegroundColor DarkGray
}

function Enable-RemoteDesktop {
    Write-Host "Enabling Remote Desktop (break-glass access)..." -ForegroundColor Cyan
    Set-ItemProperty -Path 'HKLM:\System\CurrentControlSet\Control\Terminal Server' `
        -Name 'fDenyTSConnections' -Value 0
    Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'
    Write-Host "  done. Reach it over Tailscale or 'cloudflared access tcp' -- do NOT" -ForegroundColor Green
    Write-Host "  forward 3389 from your router." -ForegroundColor Yellow
}

function Show-Summary {
    Write-Host ""
    Write-Host "Current AC power settings" -ForegroundColor Cyan
    Write-Host "-------------------------"

    # Read the values back rather than echoing what we intended to set -- a
    # powercfg call can silently no-op on some OEM power schemes.
    function Get-AcIndex($sub, $setting) {
        $line = Invoke-Powercfg /query SCHEME_CURRENT $sub $setting |
                Select-String 'Current AC Power Setting Index' |
                Select-Object -First 1
        if ($line) { ($line -split ':')[-1].Trim() } else { $null }
    }

    $lid = Get-AcIndex $SUB_BUTTONS $LID_ACTION
    $lidMeaning = switch ($lid) {
        '0x00000000' { 'Do nothing' }
        '0x00000001' { 'Sleep' }
        '0x00000002' { 'Hibernate' }
        '0x00000003' { 'Shut down' }
        default      { "unknown ($lid)" }
    }
    Write-Host ("  Lid close (AC):      {0}" -f $lidMeaning)

    $standby = Get-AcIndex $SUB_SLEEP '29f6c1db-86da-48c5-9fdb-f2b67b1f44da'
    $standbySecs = if ($standby) { [Convert]::ToInt32($standby, 16) } else { $null }
    Write-Host ("  Sleep timeout (AC):  {0}" -f $(
        if ($null -eq $standbySecs)  { 'unknown' }
        elseif ($standbySecs -eq 0)  { 'never' }
        else                         { "$($standbySecs / 60) min" }
    ))

    $net = Get-AcIndex $SUB_SLEEP $CONNECTIVITY_IN_S0
    if ($net) {
        Write-Host ("  Network in standby:  {0}" -f $(
            if ($net -eq '0x00000001') { 'enabled' } else { 'disabled' }
        ))
    }

    $wifi = Get-AcIndex $SUB_WIRELESS $WIFI_POWER_MODE
    if ($wifi) {
        Write-Host ("  Wi-Fi power saving:  {0}" -f $(
            switch ($wifi) {
                '0x00000000' { 'Maximum Performance' }
                '0x00000001' { 'Low Power Saving' }
                '0x00000002' { 'Medium Power Saving' }
                '0x00000003' { 'Maximum Power Saving' }
                default      { "unknown ($wifi)" }
            }
        ))
    }

    Write-Host ""
    Write-Host "Adapter power management" -ForegroundColor Cyan
    Write-Host "------------------------"
    Get-NetAdapter -Physical -ErrorAction SilentlyContinue |
        Where-Object { $_.Status -eq 'Up' } |
        ForEach-Object {
            $pm = Get-NetAdapterPowerManagement -Name $_.Name -ErrorAction SilentlyContinue
            Write-Host ("  {0,-14} device power-down: {1}" -f $_.Name, $(
                if ($pm) { $pm.AllowComputerToTurnOffDevice } else { 'unknown' }
            ))
        }

    Write-Host ""
    Write-Host "Reachability after this script" -ForegroundColor Cyan
    Write-Host "------------------------------"
    Write-Host "  Locked screen        -> shell/files/system/registry OK, desktop routes 503"
    Write-Host "  Logged out           -> shell/files/system/registry OK, desktop routes 503"
    Write-Host "  Lid closed on AC     -> everything OK (machine stays awake)"
    Write-Host "  Lid closed on batt.  -> sleeps, nothing reachable (deliberate)"
    Write-Host ""
    Write-Host "Check live state any time with:  GET /status" -ForegroundColor DarkGray
}

Assert-Admin
Set-PowerBehaviour
Set-AdapterPowerBehaviour
if ($EnableRdp) { Enable-RemoteDesktop }
Show-Summary

# Explicit, so the installer's "run this now" checkbox does not report a failed
# run because some powercfg call further up returned non-zero on a setting this
# machine does not have.
exit 0
