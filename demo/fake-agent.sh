#!/usr/bin/env bash
# Prints a scripted, Claude-Code-style session for the README demo. All data is fake.
d() { sleep "${1:-0.35}"; }
dim=$'\e[2m'; bold=$'\e[1m'; cyan=$'\e[36m'; green=$'\e[32m'; r=$'\e[0m'
clear
printf '%s\n\n' "${dim}~/code/acme · api-refactor${r}"
printf '%s\n\n' "${bold}> Add rate limiting to the public API and show 429s in the dashboard.${r}"; d 0.8
printf '%s\n' "${cyan}●${r} I'll add a token-bucket limiter to the API, then surface rate-limit errors in web."; d
printf '%s\n' "${cyan}●${r} ${bold}Bash${r}(git push -u origin feat/rate-limit && gh pr create --fill)"; d
printf '%s\n\n' "  ${dim}⎿${r}  ${green}https://github.com/acme/api/pull/142${r}"; d
printf '%s\n' "${cyan}●${r} ${bold}Bash${r}(cd ../web && gh pr create --title \"Show rate-limit errors in the dashboard\")"; d
printf '%s\n\n' "  ${dim}⎿${r}  ${green}https://github.com/acme/web/pull/87${r}"; d
printf '%s\n' "${cyan}●${r} Both PRs are open. web#87 is failing CI, looking into it now."
sleep 3600
