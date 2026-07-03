# gototerm の cwd 追従（OSC 7）— PowerShell 用
#
# 導入: PowerShell プロファイルに以下を貼り付けるか、この行を追記します。
#   . "path\to\osc7.ps1"
# プロファイルの場所は $PROFILE で確認できます（無ければ New-Item -Force $PROFILE）。
#
# 仕組み: プロンプトを描画するたび、現在地を OSC 7 (file://host/C:/path) で
# gototerm に通知します。これで Windows ローカルでもサイドバーが cd に追従します。

$global:__gototermPromptOrig = $function:prompt

function global:prompt {
    $esc = [char]27
    # バックスラッシュ区切りを URI 形式の / に直す（例: C:\Users\naoto → C:/Users/naoto）
    $p = $PWD.ProviderPath -replace '\\', '/'
    Write-Host -NoNewline "$esc]7;file://$env:COMPUTERNAME/$p$esc\"
    & $global:__gototermPromptOrig
}
