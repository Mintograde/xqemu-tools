# https://hub.docker.com/r/clickhouse/clickhouse-server/
$rootDir = 'V:\clickhouse-data'
$tag = 'latest'
docker stop halospawns-clickhouse
docker rm halospawns-clickhouse
docker image pull clickhouse/clickhouse-server:$tag
docker run -d -p 18123:8123 -p 19000:9000 `
    --name halospawns-clickhouse `
    --ulimit nofile=262144:262144 `
    -v "$rootDir/data:/var/lib/clickhouse/" `
    -v "$rootDir/logs:/var/log/clickhouse-server/" `
    clickhouse/clickhouse-server:$tag

# clickhouse client is runnable from wsl
#    wsl -d Ubuntu-24.04
#    cd /mnt/v/clickhouse-data
#    ./clickhouse client --host 192.168.2.10 --port 19000
# also accessible from pycharm/datagrip via localhost:18123