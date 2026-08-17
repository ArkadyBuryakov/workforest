# The publish_homebrew workflow rewrites url/sha256 on release and pushes
# this file to the ArkadyBuryakov/homebrew-tap repo; this copy is the source
# of truth for everything else (deps, completions, caveats, test).
class Workforest < Formula
  include Language::Python::Virtualenv

  desc "Git worktree forest management with per-branch setup hooks"
  homepage "https://github.com/ArkadyBuryakov/workforest"
  url "https://github.com/ArkadyBuryakov/workforest/archive/v0.2.2.tar.gz"
  sha256 "0f1800613d6fe942b0b76582e08f646a3287e28e0e6baa492b2540ad145d19de"
  license "MIT"
  head "https://github.com/ArkadyBuryakov/workforest.git", branch: "main"

  depends_on "fzf"
  depends_on "python@3.14"

  resource "pyyaml" do
    url "https://files.pythonhosted.org/packages/05/8e/961c0007c59b8dd7729d542c61a4d537767a59645b82a0b521206e1e25c2/pyyaml-6.0.3.tar.gz"
    sha256 "d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f"
  end

  def install
    virtualenv_install_with_resources

    # Completions for the plain binary; the wf shell function itself comes
    # from `workforest shell-init` (see caveats).
    bash_completion.install "src/workforest/shell/completion.bash" => "workforest"
    zsh_completion.install "completions/_workforest"

    # Reference configs, mirroring the Arch package's /usr/share/doc layout.
    (doc/"examples").install "src/workforest/examples/config.yaml"
    (doc/"examples").install "src/workforest/examples/.workforest.yaml" => "workforest.project.yaml"
  end

  def caveats
    <<~EOS
      To upgrade `wf` to a shell function (so `wf open` can change your
      shell's directory) and register completions, add this to your
      ~/.zshrc or ~/.bashrc:
        eval "$(workforest shell-init)"
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/workforest --version")
    assert_match version.to_s, shell_output("#{bin}/wf --version")
    (testpath/"init.bash").write shell_output("#{bin}/workforest shell-init bash")
    system "bash", "-n", testpath/"init.bash"
  end
end
