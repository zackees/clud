{ nixpkgs, candidate, fixture, version, digest }:
let
  pkgs = import (builtins.storePath nixpkgs) { system = builtins.currentSystem; };
  candidateFile = builtins.path { path = candidate; name = "clud-candidate"; };
  fixtureTree = builtins.path { path = fixture; name = "clud-candidate-fixture"; };
in
pkgs.testers.runNixOSTest {
  name = "clud-native-installer-${builtins.currentSystem}";
  nodes.machine = { pkgs, ... }: {
    virtualisation.memorySize = 2048;
    users.users.alice = {
      isNormalUser = true;
      createHome = true;
      home = "/home/alice";
      shell = pkgs.bash;
    };
    environment.systemPackages = [ pkgs.bash pkgs.binutils pkgs.coreutils pkgs.shadow ];
    environment.etc."clud-candidate".source = candidateFile;
    environment.etc."clud-fixture".source = fixtureTree;
    system.stateVersion = "25.05";
  };
  testScript = ''
    import json

    machine.wait_for_unit("multi-user.target")
    arch = machine.succeed("uname -m").strip()
    assert arch == "${if builtins.currentSystem == "aarch64-linux" then "aarch64" else "x86_64"}", arch
    machine.succeed("test ! -e /lib64/ld-linux-x86-64.so.2 && test ! -e /lib/ld-linux-aarch64.so.1")
    machine.fail("readelf -l /etc/clud-candidate | grep INTERP")
    machine.succeed("install -m 755 -o alice -g users /etc/clud-candidate /home/alice/clud-candidate")
    original = machine.succeed("sha256sum /home/alice/clud-candidate").split()[0]
    assert original == "${digest}", (original, "${digest}")
    machine.succeed("su - alice -c 'CLUD_INSTALLER_CI_FIXTURE_DIR=/etc/clud-fixture HTTPS_PROXY=http://127.0.0.1:1 /home/alice/clud-candidate --installer --install-current --yes'")
    selected = machine.succeed("su - alice -c 'bash -lc \"command -v clud; clud --version\"'").strip().splitlines()
    assert len(selected) == 2, selected
    assert selected[0] == "/home/alice/.local/bin/clud", selected
    assert selected[1] == "clud ${version}", selected
    installed = machine.succeed("sha256sum /home/alice/.local/bin/clud").split()[0]
    assert installed == original, (installed, original)
    machine.fail("su - alice -c 'CLUD_INSTALLER_CI_FIXTURE_DIR=/etc/clud-fixture /home/alice/clud-candidate --installer --install-version 2.8.13 --yes'")
    assert machine.succeed("sha256sum /home/alice/.local/bin/clud").split()[0] == original
    print("NIXOS_EVIDENCE " + json.dumps({"host_arch": arch, "payload_arch": arch, "sha256": installed, "version": selected[1], "resolved_path": selected[0]}, sort_keys=True))
  '';
}
