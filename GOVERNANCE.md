# Project Governance

RMUX is maintained under a lead-maintainer model.

## Roles

- **Lead maintainer**: Sidney Sissaoui (@shideneyu). Owns release decisions,
  project direction, repository administration, package publication, and final
  review decisions.
- **Committers**: trusted contributors with repository write access. Committers
  may merge reviewed changes within the areas agreed with the lead maintainer.
- **Contributors**: anyone who reports issues, proposes changes, submits pull
  requests, reviews work, or improves documentation.

## Decision Making

Routine changes are handled through GitHub issues and pull requests. The lead
maintainer has final decision authority when there is disagreement or when a
change affects release quality, security, compatibility, or project scope.

Security fixes, release automation, package publication, and credential access
are handled conservatively. Changes in those areas require maintainer review.

## Committer Access

Committer access is granted by the lead maintainer after sustained high-quality
contributions. Committers must use two-factor authentication on GitHub. Anyone
who can publish crates or package-manager artifacts must also use two-factor
authentication for those services when available.

## Continuity

The project keeps a private continuity procedure for emergency access to the
organization, release credentials, domains, package-manager accounts, and
security contact channels. The procedure is intentionally not stored in this
repository.

The continuity goal is to keep issue triage, security patches, pull request
review, and releases possible within one week if the lead maintainer is
unavailable.
