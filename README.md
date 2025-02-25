## Quickstart

- Start database using vagrant `vagrant up`
- Add launch params to xemu.exe shortcut `-s -qmp tcp:localhost:4444,server,nowait`

## Notes

Just dumping some early 2019 experiments based on JayFox/xbox7887/Cyrix's stuff

`halo_basic_stat.py` starts a websocket server for live data, which can be read by halospawns-live branch.

Most of the requirements/imports are not required...


## TimescaleDB setup (Vagrant/Docker):

Official docs for timescaledb docker configuration https://docs.timescale.com/latest/getting-started/installation/docker/installation-docker

Note: timescale-timescaledb-postgis image is no longer maintained. Use https://hub.docker.com/r/timescale/timescaledb-ha

```
# install docker on vm, run timescaledb on docker
vagrant up

# connect to vm
vagrant ssh

# enter psql on docker container
docker exec -it timescale-timescaledb-postgis psql -U postgres

# list installed extensions
\dx

# install postgis extension if not installed
CREATE EXTENSION IF NOT EXISTS postgis;

# set up password
# TODO: do this in a secure manner
ALTER USER postgres PASSWORD 'postgres';

# exit psql
\q
```

## Some wishlist items
- track non-player objects in database as well (thrown grenades, dropped/naded weapons, fired rockets, etc)
- user-defined map-specific regions to query against (e.g. "dammy pit" or "hangem trench")