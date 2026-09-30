@echo off
docker compose run --rm -v "%CD%:/workspace" -w /workspace client %*
