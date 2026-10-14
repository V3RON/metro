# Phase approvals

Only Szymon adds files here. `phase-<n>.md` approves phase n after he has read
`port/reports/phase-<n>.md`; its presence lets `./gate.sh` move on to phase n+1.
Write whether the phase is approved and any conditions for the next one.
Agents never create, edit or delete files in this directory (see RULES.md).
