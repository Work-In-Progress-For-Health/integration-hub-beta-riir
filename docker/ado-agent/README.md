# Self-hosted Azure DevOps agent

Implements `spec/21-local-ci.md` §11. **Read §11.2 before registering it.**

An ephemeral agent: each container registers, runs **one** job, and exits.

```sh
podman build -f docker/ado-agent/Containerfile \
  --build-arg AGENT_ARCH=arm64 -t ihb-rs/ado-agent:dev docker/ado-agent   # x64 for amd64

# The token comes from your environment, never from a file in this repository.
podman run --rm \
  -e AZP_URL=https://dev.azure.com/<organisation> \
  -e AZP_POOL=<pool> \
  -e AZP_TOKEN \
  ihb-rs/ado-agent:dev
```

`AZP_TOKEN` is a PAT scoped to **Agent Pools (read, manage)** only. The agent
drops it from its environment before the job starts.

## What it can and cannot do

- It builds and tests the Rust workspace (the `CodeQuality` stage).
- **It cannot run the `docker-build-push` stages.** Those need a container
  engine inside the agent, which means mounting a socket into a container that
  runs pipeline code. That is a privilege decision for the Hub team, not a
  default (§11.4).
- Offline agents from a crashed container stay listed in the pool until
  removed; `--replace` reuses the name, so a stable `AZP_AGENT_NAME` avoids a
  pile of them.
