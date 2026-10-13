Velnor orb export

This export contains both commits after origin/main, including orb setup and the Docker/Mr Boxington migration.

Preferred import into a Velnor clone:
  git fetch ./velnor-changes.bundle perf/docker-rust-mbx:perf/docker-rust-mbx
  git switch perf/docker-rust-mbx

Alternative patch import from a clean origin/main checkout:
  git am velnor-changes.patch
