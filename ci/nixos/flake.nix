{
  description = "Pinned NixOS VM fixture for native clud installer acceptance";

  inputs.nixpkgs.url = "tarball+https://codeload.github.com/NixOS/nixpkgs/tar.gz/ac62194c3917d5f474c1a844b6fd6da2db95077d";

  outputs = { self, nixpkgs }: { };
}
