# Requeue previously submitted read receipts; never mark newer messages read.
# Run after starting the local acks service. No credentials or message bodies are logged.
$ErrorActionPreference = 'Stop'
$pending = @()
$keys = @(docker exec zarzour-backend-redis-1 redis-cli --scan --pattern 'acker:*')
if ($LASTEXITCODE -ne 0) { throw 'Cannot scan pending receipts' }
foreach ($receiptKey in $keys) {
    if ($receiptKey -notmatch '^acker:([0-9A-HJKMNP-TV-Z]{26})\+([0-9A-HJKMNP-TV-Z]{26})$') { continue }
    $receiptUser = $Matches[1]
    $receiptChannel = $Matches[2]
    $receiptMessage = docker exec zarzour-backend-redis-1 redis-cli --raw GET $receiptKey
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read pending receipt' }
    if ($receiptMessage -notmatch '^[0-9A-HJKMNP-TV-Z]{26}$') { continue }
    $pending += @{ user = $receiptUser; channel = $receiptChannel; message = $receiptMessage }
    $payload = @{ user_id = $receiptUser; channel_id = $receiptChannel; server_id = $null } | ConvertTo-Json -Compress
    $payload | docker exec -i zarzour-backend-rabbit-1 sh -c 'RABBITMQADMIN_USERNAME="$RABBITMQ_DEFAULT_USER" RABBITMQADMIN_PASSWORD="$RABBITMQ_DEFAULT_PASS" rabbitmqadmin --quiet --non-interactive publish message --routing-key internal.ack --payload-file -' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Failed to requeue pending receipt' }
}
Write-Output "Requeued receipts: $($pending.Count)"
Start-Sleep -Seconds 2
$pendingJson = ConvertTo-Json -InputObject @($pending) -Compress
$verification = 'const receipts = ' + $pendingJson + '; const col = db.getSiblingDB("revolt").channel_unreads; let saved = 0; for (const receipt of receipts) { const row = col.findOne({"_id.user": receipt.user, "_id.channel": receipt.channel}); if (row && row.last_id >= receipt.message && !(row.mentions || []).some(id => id <= receipt.message)) saved++; } print(JSON.stringify({submitted: receipts.length, persisted: saved})); if (saved !== receipts.length) quit(1);'
$verification | docker exec -i zarzour-backend-database-1 mongosh --quiet
if ($LASTEXITCODE -ne 0) { throw 'Receipt persistence verification failed' }
