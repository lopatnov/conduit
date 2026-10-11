---
description: Find the uncovered lines behind a failing SonarCloud quality gate on a PR
---

The project is public, so a local session can list the uncovered new lines without the web UI (replace `<n>` with the PR number):

```
curl 'https://sonarcloud.io/api/measures/component_tree?component=lopatnov_conduit&pullRequest=<n>&metricKeys=new_uncovered_lines,new_coverage&strategy=leaves&ps=100'
curl 'https://sonarcloud.io/api/sources/lines?key=<fileKey>&pullRequest=<n>'   # per-line coverage of one file
```

A cloud container's proxy blocks the host (`CONNECT` 403, `ENOTFOUND`; checked 2026-10-11). There, use the `mcp__sonarqube__*` connector if it is present, otherwise ask the owner for the list. Do not merge with a red gate without the owner's explicit yes.

If the coverage figure does not move after new tests, the uncovered lines are elsewhere (for example Unix-only or server wiring code that unit tests do not reach); look at the list before adding more tests.
