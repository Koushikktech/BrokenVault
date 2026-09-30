#!/usr/bin/env bash
set -euo pipefail

TEAM="${1:-team}"
COMMIT=$(git rev-parse HEAD)
git archive --format=zip -o "${TEAM}-brokenvault.zip" "${COMMIT}"
echo "Created ${TEAM}-brokenvault.zip from commit ${COMMIT}"
