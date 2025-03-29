# Dynamically determine the script's directory
$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Definition
Set-Location -Path $scriptDirectory

# Run 'yarn start' in a separate process (without waiting for it to return)
Write-Output "Running application server..."
$yarnPath = Join-Path $env:APPDATA "\npm\node_modules\yarn\bin\yarn.js"
$yarnProcess = Start-Process "node" -ArgumentList $yarnPath, "preview" -NoNewWindow -PassThru

Write-Output "Waiting for 5 seconds before opening the browser..."
Start-Sleep -Seconds 5

Write-Output "Opening browser to http://localhost:4173/cymatics in full screen..."
Start-Process "chrome.exe" -ArgumentList "--app=http://localhost:4173/cymatics", "--start-fullscreen"

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
