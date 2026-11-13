# LazyTerm 0.6.0 Beta 1

This is a beta release candidate for the integrated Hosts, Files, Tunnels, Snippets, Runtime, Projects, Credentials and Settings workspace.

- Configuration import/export merges OpenSSH stanzas and LazyTerm metadata; credential import/export is separately encrypted.
- Shared SSH password prompts cover native SSH, embedded/native SFTP, ProxyJump, Tunnels, Snippets and app-scoped remote Projects.
- Ownership guards clean app-scoped processes after forced exit; persistent Project runners can be reattached and killed-runner records recovered.
- One macOS Universal Binary package runs natively on Intel and Apple Silicon and includes provenance for both builds; SHA256SUMS and a generated Homebrew Formula accompany the archive.
- Version/help/diagnostics commands work without entering the terminal UI or reading configuration.

Requirements, installation, backup and upgrade: see INSTALL.md in each package. Security boundaries and remaining release gates: see SECURITY-REVIEW.md. Complete manual acceptance and native architecture checks before publishing this draft. Full localization, first-use host-key dialogs in background workflows, encrypted-key passphrase storage, remote persistent runners and Windows support are not included. Existing configuration and backups should be retained before upgrading.
