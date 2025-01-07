$temp = "C:\Temp"
$tempServices = "C:\Temp\iceoryx2\services"

Stop-Process -Name "iceoryx2-request-response" -ErrorAction Ignore

$stateFiles = Get-ChildItem -Path $temp -Filter "iox2_*.shm_state"
$serviceFiles = Get-ChildItem -Path $tempServices -Filter "iox2_*.service"

($stateFiles + $serviceFiles) | Where-Object{ $_ -ne $null } | ForEach-Object {
    Remove-Item -Path $_.FullName -Force
}
