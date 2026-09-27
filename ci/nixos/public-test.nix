{ nixpkgs, tag, version, digest, mode }:
let
  pkgs = import (builtins.storePath nixpkgs) { system = builtins.currentSystem; };
  arch = if builtins.currentSystem == "aarch64-linux" then "aarch64" else "x86_64";
in
assert mode == "candidate" || mode == "released";
pkgs.testers.runNixOSTest {
  name = "clud-public-installer-${mode}-${builtins.currentSystem}";
  nodes.machine = { pkgs, ... }: {
    virtualisation.memorySize = 2048;
    virtualisation.vlans = [];
    virtualisation.qemu.options = [
      "-netdev user,id=publicnet"
      "-device virtio-net-pci,netdev=publicnet,mac=52:54:00:ca:fe:01"
    ];
    networking.useDHCP = true;
    environment.stub-ld.enable = false;
    programs.nix-ld.enable = false;
    users.users.alice = {
      isNormalUser = true;
      createHome = true;
      home = "/home/alice";
      shell = pkgs.bash;
    };
    environment.systemPackages = [ pkgs.bash pkgs.binutils pkgs.coreutils pkgs.curl pkgs.gnugrep pkgs.gnused pkgs.jq pkgs.shadow ];
    environment.etc."clud-public-guest".source = ./public-guest.sh;
    system.stateVersion = "25.05";
  };
  testScript = ''
    machine.wait_for_unit("multi-user.target")
    output = machine.succeed("bash /etc/clud-public-guest ${mode} ${tag} ${version} ${digest} ${arch}")
    assert "PUBLIC_NIXOS_EVIDENCE " in output, output
    print(output)
  '';
}
