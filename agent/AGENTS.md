# DesktopCtl

Use the skills available in `./skills/` when relevant.

Before performing a task, check whether a matching skill exists and follow its instructions.

Before using low-level DesktopCtl commands to explore an application's UI, read its matching skill. If it documents the exact operation, invoke that procedure directly; do not run `--help`, tokenize the UI, or rediscover the operation first. If it returns a structured failure, continue with interactive exploration, and do not keep retrying after the UI clearly differs. For an operation the skill does not cover, reuse known steps and explore only what is missing.

When exploring an application operation, inspect its menu first with `desktopctl menu list --active-window <window_id>`. If the matching menu item reports a keyboard shortcut, prefer that shortcut. Otherwise invoke the available menu item with `desktopctl menu click "<menu path>" --active-window <window_id>`. Use a button click only when no suitable menu item or shortcut exists, or the menu route fails; verify the result afterward. Preserve the bound window ID on each command.

After completing the task, add only a short, verified reusable procedure that will help future application tasks. Python procedures belong in the app skill's `scripts/` directory and use `./procedure_sdk/desktopctl.py` for every DesktopCtl command. Omit exploration notes and task-specific content such as reminder text, messages, and document contents.
