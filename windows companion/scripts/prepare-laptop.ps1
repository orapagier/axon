<#
.SYNOPSIS
    Prepares a laptop to stay reachable for remote automation.

.DESCRIPTION
    The service handles being locked. It cannot do anything about being asleep —
    a sleeping laptop has no network, so the Cloudflare tunnel drops and every
    route fails regardless of how the API is architected. On a laptop this is
    the failure you will actually hit, and it is pure configuration.

    Changes made (all scoped to AC power, so battery behaviour is untouched):
      - never sleep or hibernate while plugged in
      - closing the lid while plugged in does nothing
      - keep the network up during connected standby
      - display still sleeps, which is fine and saves the panel

    Optionally enables Remote Desktop as a break-glass path. Worth having: when
    the automation layer wedges, you need a way in that does not depend on the
    automation layer. It is opt-in because it widens the machine's attack
    surface, and should only ever be reachable over Tailscale, a Cloudflare
    tunnel, or a VPN — never a forwarded port on your router.

.PARAMETER EnableRdp
    Also enable the Remote Desktop host and its firewall rules.

.PARAMETER Revert
    Restore Windows' default AC power behaviour (sleep after 30 min, lid sleeps).

.EXAMPLE
    .\prepare-laptop.ps1
    .\prepare-laptop.ps1 -EnableRdp
    .\prepare-laptop.ps1 -Revert
#>

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
        powercfg /change standby-timeout-ac 30
        powercfg /change hibernate-timeout-ac 180
        # 1 = Sleep
        powercfg /setacvalueindex SCHEME_CURRENT $SUB_BUTTONS $LID_ACTION 1
    }
    else {
        Write-Host "Keeping the machine awake on AC..." -ForegroundColor Cyan
        # 0 = never
        powercfg /change standby-timeout-ac 0
        powercfg /change hibernate-timeout-ac 0
        # The display can still sleep — it does not affect the tunnel, and the
        # desktop agent keeps capturing because the session stays active.
        powercfg /change monitor-timeout-ac 15
        # 0 = Do nothing on lid close
        powercfg /setacvalueindex SCHEME_CURRENT $SUB_BUTTONS $LID_ACTION 0
        # 1 = network stays connected in modern standby
        powercfg /setacvalueindex SCHEME_CURRENT $SUB_SLEEP $CONNECTIVITY_IN_S0 1
    }

    powercfg /setactive SCHEME_CURRENT
    Write-Host "  done." -ForegroundColor Green
}

function Enable-RemoteDesktop {
    Write-Host "Enabling Remote Desktop (break-glass access)..." -ForegroundColor Cyan
    Set-ItemProperty -Path 'HKLM:\System\CurrentControlSet\Control\Terminal Server' `
        -Name 'fDenyTSConnections' -Value 0
    Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'
    Write-Host "  done. Reach it over Tailscale or 'cloudflared access tcp' — do NOT" -ForegroundColor Green
    Write-Host "  forward 3389 from your router." -ForegroundColor Yellow
}

function Show-Summary {
    Write-Host ""
    Write-Host "Current AC power settings" -ForegroundColor Cyan
    Write-Host "-------------------------"

    # Read the values back rather than echoing what we intended to set — a
    # powercfg call can silently no-op on some OEM power schemes.
    function Get-AcIndex($sub, $setting) {
        $line = powercfg /query SCHEME_CURRENT $sub $setting |
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
if ($EnableRdp) { Enable-RemoteDesktop }
Show-Summary
