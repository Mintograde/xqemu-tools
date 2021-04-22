# -*- mode: ruby -*-
# vi: set ft=ruby :
Vagrant.configure("2") do |config|

    # https://app.vagrantup.com/ubuntu/boxes/xenial64
    config.vm.box = "ubuntu/xenial64"

    config.vm.network "forwarded_port", guest: 5432, host: 5432, host_ip: "127.0.0.1"

    # https://hub.docker.com/r/timescale/timescaledb-postgis
    config.vm.provision "docker" do |d|
        d.run "timescale/timescaledb-postgis",
            args: "-p 5432:5432 --env TIMESCALEDB_TELEMETRY=off"
    end

end
