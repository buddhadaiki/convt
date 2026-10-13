---
"@convt/desktop": patch
"@convt/cli": patch
"@convt/web": patch
---

On Windows, the installer now works on PCs that keep PowerShell's default script policy, where installing or uninstalling convt used to fail and roll back. The app, its installer and Apps & features use the convt mark, the same icon as on macOS, Linux and convt.app. Each installer page is headed by that icon, and the last page has one Concluir button with an "Abrir o convt agora" choice. Right-clicking a file in Explorer starts convt's menu check once instead of twice.
