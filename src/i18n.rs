use crate::metadata::Language;

pub fn text(language: Language, english: &str) -> &str {
    if language == Language::English {
        return english;
    }
    match (language, english) {
        (Language::Chinese, "Hosts") => "主机",
        (Language::Japanese, "Hosts") => "ホスト",
        (Language::Chinese, "Files") => "文件",
        (Language::Japanese, "Files") => "ファイル",
        (Language::Chinese, "Tunnels") => "隧道",
        (Language::Japanese, "Tunnels") => "トンネル",
        (Language::Chinese, "Snippets") => "命令片段",
        (Language::Japanese, "Snippets") => "スニペット",
        (Language::Chinese, "Runtime") => "运行状态",
        (Language::Japanese, "Runtime") => "実行状況",
        (Language::Chinese, "Projects") => "项目",
        (Language::Japanese, "Projects") => "プロジェクト",
        (Language::Chinese, "Credentials") => "凭据",
        (Language::Japanese, "Credentials") => "認証情報",
        (Language::Chinese, "Settings") => "设置",
        (Language::Japanese, "Settings") => "設定",
        (Language::Chinese, "Theme") => "主题",
        (Language::Japanese, "Theme") => "テーマ",
        (Language::Chinese, "Language") => "语言",
        (Language::Japanese, "Language") => "言語",
        (Language::Chinese, "Forest") => "森林绿",
        (Language::Japanese, "Forest") => "フォレスト",
        (Language::Chinese, "Ocean") => "海洋蓝",
        (Language::Japanese, "Ocean") => "オーシャン",
        (Language::Chinese, "Amber") => "琥珀金",
        (Language::Japanese, "Amber") => "アンバー",
        (Language::Chinese, "Rose") => "玫瑰红",
        (Language::Japanese, "Rose") => "ローズ",
        (Language::Chinese, "true") => "开启",
        (Language::Japanese, "true") => "オン",
        (Language::Chinese, "false") => "关闭",
        (Language::Japanese, "false") => "オフ",
        (Language::Chinese, "Show Hosts tab") => "显示主机页签",
        (Language::Japanese, "Show Hosts tab") => "ホストタブを表示",
        (Language::Chinese, "Show Files tab") => "显示文件页签",
        (Language::Japanese, "Show Files tab") => "ファイルタブを表示",
        (Language::Chinese, "Show Tunnels tab") => "显示隧道页签",
        (Language::Japanese, "Show Tunnels tab") => "トンネルタブを表示",
        (Language::Chinese, "Show Snippets tab") => "显示命令片段页签",
        (Language::Japanese, "Show Snippets tab") => "スニペットタブを表示",
        (Language::Chinese, "Show Runtime tab") => "显示运行状态页签",
        (Language::Japanese, "Show Runtime tab") => "実行状況タブを表示",
        (Language::Chinese, "Show Projects tab") => "显示项目页签",
        (Language::Japanese, "Show Projects tab") => "プロジェクトタブを表示",
        (Language::Chinese, "Show Credentials tab") => "显示凭据页签",
        (Language::Japanese, "Show Credentials tab") => "認証情報タブを表示",
        (Language::Chinese, "Export configuration") => "导出配置",
        (Language::Japanese, "Export configuration") => "設定をエクスポート",
        (Language::Chinese, "Export credentials") => "导出凭据",
        (Language::Japanese, "Export credentials") => "認証情報をエクスポート",
        (Language::Chinese, "Press Enter") => "按 Enter",
        (Language::Japanese, "Press Enter") => "Enter を押す",
        (Language::Chinese, "Encrypted .age file") => "加密的 .age 文件",
        (Language::Japanese, "Encrypted .age file") => "暗号化 .age ファイル",
        (Language::Chinese, "remote workspace") => "远程工作区",
        (Language::Japanese, "remote workspace") => "リモートワークスペース",
        (Language::Chinese, "[,] Settings") => "[,] 设置",
        (Language::Japanese, "[,] Settings") => "[,] 設定",
        (Language::Chinese, "Enter / Space change  Tab switch tab  q quit") => {
            "Enter / Space 修改  Tab 返回  q 退出"
        }
        (Language::Japanese, "Enter / Space change  Tab switch tab  q quit") => {
            "Enter / Space 変更  Tab 戻る  q 終了"
        }
        (Language::Chinese, "PORT FORWARDING") => "端口转发",
        (Language::Japanese, "PORT FORWARDING") => "ポートフォワーディング",
        (Language::Chinese, "EDIT PORT FORWARDING") => "编辑端口转发",
        (Language::Japanese, "EDIT PORT FORWARDING") => "ポート転送を編集",
        (Language::Chinese, "No forwarding selected.") => "未选择转发规则。",
        (Language::Japanese, "No forwarding selected.") => "転送ルールが選択されていません。",
        (Language::Chinese, "FILES") => "文件",
        (Language::Japanese, "FILES") => "ファイル",
        (Language::Chinese, "LOCAL FILES") => "本地文件",
        (Language::Japanese, "LOCAL FILES") => "ローカルファイル",
        (Language::Chinese, "REMOTE FILES") => "远程文件",
        (Language::Japanese, "REMOTE FILES") => "リモートファイル",
        (Language::Chinese, "TRANSFERS") => "传输任务",
        (Language::Japanese, "TRANSFERS") => "転送タスク",
        (Language::Chinese, "Select a Host") => "选择主机",
        (Language::Japanese, "Select a Host") => "ホストを選択",
        (Language::Chinese, "RUNTIME") => "运行状态",
        (Language::Japanese, "RUNTIME") => "実行状況",
        (Language::Chinese, "LISTENERS") => "监听端口",
        (Language::Japanese, "LISTENERS") => "リスナー",
        (Language::Chinese, "PROCESS") => "进程",
        (Language::Japanese, "PROCESS") => "プロセス",
        (Language::Chinese, "Port inspection failed") => "端口检查失败",
        (Language::Japanese, "Port inspection failed") => "ポートの確認に失敗しました",
        (Language::Chinese, "No listeners in this category.") => "此分类下没有监听端口。",
        (Language::Japanese, "No listeners in this category.") => {
            "このカテゴリにリスナーはありません。"
        }
        (Language::Chinese, "PROJECTS") => "项目",
        (Language::Japanese, "PROJECTS") => "プロジェクト",
        (Language::Chinese, "DETAILS") => "详情",
        (Language::Japanese, "DETAILS") => "詳細",
        (Language::Chinese, "Connection") => "连接",
        (Language::Japanese, "Connection") => "接続",
        (Language::Chinese, "Recent sessions") => "最近会话",
        (Language::Japanese, "Recent sessions") => "最近のセッション",
        (Language::Chinese, "Host details") => "主机详情",
        (Language::Japanese, "Host details") => "ホストの詳細",
        (Language::Chinese, "Snippets in package") => "命令包中的片段",
        (Language::Japanese, "Snippets in package") => "パッケージ内のスニペット",
        (Language::Chinese, "Snippet execution") => "命令执行",
        (Language::Japanese, "Snippet execution") => "スニペット実行",
        (Language::Chinese, "Target hosts") => "目标主机",
        (Language::Japanese, "Target hosts") => "対象ホスト",
        (Language::Chinese, "Targets") => "目标",
        (Language::Japanese, "Targets") => "対象",
        (Language::Chinese, "No projects.") => "没有项目。",
        (Language::Japanese, "No projects.") => "プロジェクトがありません。",
        (Language::Chinese, "No commands or links.") => "没有命令或网页入口。",
        (Language::Japanese, "No commands or links.") => "コマンドまたはリンクがありません。",
        (Language::Chinese, "SNIPPETS") => "命令片段",
        (Language::Japanese, "SNIPPETS") => "スニペット",
        (Language::Chinese, "PACKAGES") => "命令包",
        (Language::Japanese, "PACKAGES") => "パッケージ",
        (Language::Chinese, "RUNS") => "执行记录",
        (Language::Japanese, "RUNS") => "実行履歴",
        (Language::Chinese, "OUTPUT") => "输出",
        (Language::Japanese, "OUTPUT") => "出力",
        (Language::Chinese, "KEYCHAIN & SSH KEYS") => "钥匙串与 SSH 密钥",
        (Language::Japanese, "KEYCHAIN & SSH KEYS") => "キーチェーンと SSH 鍵",
        (Language::Chinese, "KEYCHAIN") => "钥匙串",
        (Language::Japanese, "KEYCHAIN") => "キーチェーン",
        (Language::Chinese, "SSH KEYS") => "SSH 密钥",
        (Language::Japanese, "SSH KEYS") => "SSH 鍵",
        (Language::Chinese, "No credentials found.") => "没有找到凭据。",
        (Language::Japanese, "No credentials found.") => "認証情報が見つかりません。",
        (Language::Chinese, "Details") => "详情",
        (Language::Japanese, "Details") => "詳細",
        (Language::Chinese, "Local") => "本机",
        (Language::Japanese, "Local") => "ローカル",
        (Language::Chinese, "Hosts ▾") => "主机 ▾",
        (Language::Japanese, "Hosts ▾") => "ホスト ▾",
        (Language::Chinese, "All hosts") => "全部主机",
        (Language::Japanese, "All hosts") => "すべてのホスト",
        (Language::Chinese, "Group") => "分组",
        (Language::Japanese, "Group") => "グループ",
        (Language::Chinese, "Target") => "目标",
        (Language::Japanese, "Target") => "対象",
        (Language::Chinese, "Alias") => "别名",
        (Language::Japanese, "Alias") => "エイリアス",
        (Language::Chinese, "Command") => "命令",
        (Language::Japanese, "Command") => "コマンド",
        (Language::Chinese, "Status") => "状态",
        (Language::Japanese, "Status") => "状態",
        (Language::Chinese, "Working directory") => "工作目录",
        (Language::Japanese, "Working directory") => "作業ディレクトリ",
        (Language::Chinese, "Password") => "密码",
        (Language::Japanese, "Password") => "パスワード",
        (Language::Chinese, "Username") => "用户名",
        (Language::Japanese, "Username") => "ユーザー名",
        (Language::Chinese, "Process") => "进程",
        (Language::Japanese, "Process") => "プロセス",
        (Language::Chinese, "Project") => "项目",
        (Language::Japanese, "Project") => "プロジェクト",
        (Language::Chinese, "Port") => "端口",
        (Language::Japanese, "Port") => "ポート",
        (Language::Chinese, "Machine") => "机器",
        (Language::Japanese, "Machine") => "マシン",
        (Language::Chinese, "Protocol") => "协议",
        (Language::Japanese, "Protocol") => "プロトコル",
        (Language::Chinese, "Address") => "地址",
        (Language::Japanese, "Address") => "アドレス",
        (Language::Chinese, "User") => "用户",
        (Language::Japanese, "User") => "ユーザー",
        (Language::Chinese, "Class") => "类别",
        (Language::Japanese, "Class") => "分類",
        (Language::Chinese, "Origin") => "来源",
        (Language::Japanese, "Origin") => "起動元",
        (Language::Chinese, "Command name") => "命令名称",
        (Language::Japanese, "Command name") => "コマンド名",
        (Language::Chinese, "Base directory") => "基础目录",
        (Language::Japanese, "Base directory") => "ベースディレクトリ",
        (Language::Chinese, "Details:") => "详情：",
        (Language::Japanese, "Details:") => "詳細：",
        _ => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_primary_navigation_and_settings_in_all_locales() {
        assert_eq!(text(Language::English, "Hosts"), "Hosts");
        assert_eq!(text(Language::Chinese, "Hosts"), "主机");
        assert_eq!(text(Language::Japanese, "Hosts"), "ホスト");
        assert_eq!(
            text(Language::Japanese, "Show Credentials tab"),
            "認証情報タブを表示"
        );
        assert_eq!(text(Language::Chinese, "untranslated"), "untranslated");
    }

    #[test]
    fn all_language_choices_are_distinct_and_cycle() {
        assert_eq!(Language::English.next(), Language::Chinese);
        assert_eq!(Language::Chinese.next(), Language::Japanese);
        assert_eq!(Language::Japanese.next(), Language::English);
        for language in [Language::English, Language::Chinese, Language::Japanese] {
            let encoded = serde_json::to_string(&language).unwrap();
            assert_eq!(
                serde_json::from_str::<Language>(&encoded).unwrap(),
                language
            );
        }
    }
}
