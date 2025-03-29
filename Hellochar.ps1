# Navigate to the target directory
Set-Location -Path "C:\Users\Richd\OneDrive\Documents\Hellochar\hellochar2024"

# Run 'yarn start' in a separate process (without waiting for it to return)
Write-Output "Running application server..."
$yarnProcess = Start-Process "yarn" -ArgumentList "preview" -NoNewWindow -PassThru

Write-Output "Waiting for 5 seconds before opening the browser..."
Start-Sleep -Seconds 5

Write-Output "Opening browser to http://localhost:4173/cymatics..."
Start-Process "http://localhost:4173/cymatics"

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
