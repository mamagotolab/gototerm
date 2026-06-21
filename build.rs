fn main() {
    // Windows ビルド時に exe へアイコンを埋め込む（フラスコのアイコン）。
    // ホストが Windows のときだけ winresource を使う（Linux などでは何もしない）。
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/gototerm.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=アイコンの埋め込みに失敗: {e}");
        }
    }
}
