# Dynamically determine the script's directory
$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Definition
Set-Location -Path $scriptDirectory

# Run 'yarn start' in a separate process (without waiting for it to return)
Write-Output "Running application server..."
$yarnPath = Join-Path $env:APPDATA "\npm\node_modules\yarn\bin\yarn.js"
$yarnProcess = Start-Process "node" -ArgumentList $yarnPath, "preview" -NoNewWindow -PassThru

Write-Output "Waiting for 2 seconds before opening the browser..."
Start-Sleep -Seconds 2

Write-Output "Opening browser to http://localhost:4173/cymatics in full screen..."
Start-Process "chrome.exe" -ArgumentList "--app=http://localhost:4173/cymatics", "--start-fullscreen", "--autoplay-policy=no-user-gesture-required"

# Wait for Chrome to launch
Start-Sleep -Seconds 2

# Send F11 to toggle fullscreen in the window manager
# Add-Type -AssemblyName System.Windows.Forms
# [System.Windows.Forms.SendKeys]::SendWait("{F11}")

# Wait for the user to close the terminal
Write-Output "Script running. Close the terminal to stop 'yarn preview'."
try {
    # Wait indefinitely until the terminal is closed
    while ($true) {
        Start-Sleep -Seconds 1
    }
} finally {
    # Ensure the yarn process is terminated when the script ends
    if ($yarnProcess -and !$yarnProcess.HasExited) {
        Write-Output "Stopping 'yarn preview'..."
        Stop-Process -Id $yarnProcess.Id -Force
    }
    Write-Output "Script complete."
}
