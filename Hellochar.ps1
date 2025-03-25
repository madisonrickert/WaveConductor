# Navigate to the target directory
Set-Location -Path "C:\Users\LoveTech\Downloads\hellochar.com-master"

# Run 'yarn' command
Write-Output "Running 'yarn'..."
try {
    yarn
} catch {
    Write-Output "Yarn failed as expected with error."
}

# Wait for 3 seconds
Write-Output "Waiting for 3 seconds..."
Start-Sleep -Seconds 3

# Run 'yarn start' in a separate process (without waiting for it to return)
Write-Output "Running 'yarn start'..."
Start-Process "yarn" -ArgumentList "start"

# Wait for 15 seconds before opening the browser
Write-Output "Waiting for 15 seconds before opening the browser..."
Start-Sleep -Seconds 15

# Open the default web browser to the specified address. Server link (10.0.0.XXX) may change with every yarn start, update this script as necessary.
Write-Output "Opening browser to http://10.0.0.169:8080/line..."
Start-Process "http://10.0.0.169:8080/line"

# Keep the PowerShell window open
Write-Output "Script complete. Press Enter to exit."
Read-Host
