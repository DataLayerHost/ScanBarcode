param([int]$ProcessToWait, [string]$Staged, [string]$Destination, [string]$Backup)
while (Get-Process -Id $ProcessToWait -ErrorAction SilentlyContinue) { Start-Sleep -Milliseconds 100 }
if (Test-Path -LiteralPath $Backup) { exit 1 }
try {
	Move-Item -LiteralPath $Destination -Destination $Backup -ErrorAction Stop
	Move-Item -LiteralPath $Staged -Destination $Destination -ErrorAction Stop
	Start-Process -FilePath $Destination
	Remove-Item -LiteralPath $Backup -Force
} catch {
	if (Test-Path -LiteralPath $Backup) {
		if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
		Move-Item -LiteralPath $Backup -Destination $Destination
		Start-Process -FilePath $Destination
	}
}
