#!/usr/bin/env bash
# Register one ephemeral agent, run one job, exit (spec/21 §11.3).
#
# Required environment:
#   AZP_URL    https://dev.azure.com/<organisation>
#   AZP_POOL   the agent pool's name
#   AZP_TOKEN  a PAT with "Agent Pools (read, manage)" and nothing else
# Optional:
#   AZP_AGENT_NAME  default: the host name
set -euo pipefail

for variable in AZP_URL AZP_POOL AZP_TOKEN; do
  if [ -z "${!variable:-}" ]; then
    echo "start.sh: ${variable} is not set (see docker/ado-agent/README.md)" >&2
    exit 64
  fi
done

cd "$(dirname "$0")"

./config.sh --unattended \
  --url "${AZP_URL}" \
  --auth pat --token "${AZP_TOKEN}" \
  --pool "${AZP_POOL}" \
  --agent "${AZP_AGENT_NAME:-$(hostname)}" \
  --replace --acceptTeeEula

# The job must not hold the credential that registered the agent
# (spec/21 §5.2): drop it before any pipeline script can read it.
unset AZP_TOKEN

# `--once`: take one job, then exit, so the next job starts from a clean
# container (`podman run --rm`).
exec ./run.sh --once
