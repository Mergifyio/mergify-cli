
def make_dist():
    return default_python_distribution(python_version="3.10")

def make_exe(dist):
    policy = dist.make_python_packaging_policy()
    policy.bytecode_optimize_level_two = True
    python_config = dist.make_python_interpreter_config()
    python_config.allocator_debug = False
    python_config.run_command = "from git_push_stack import cli; cli()"

    exe = dist.to_python_executable(
        name="git-push-stack",
        packaging_policy=policy,
        config=python_config,
    )
    exe.add_python_resources(
        exe.pip_install(["dist/git_push_stack-0.1.1-py3-none-any.whl"])
    )
    return exe

def make_embedded_resources(exe):
    return exe.to_embedded_resources()

def make_install(exe):
    files = FileManifest()
    files.add_python_resource(".", exe)
    return files

register_target("dist", make_dist)
register_target("exe", make_exe, depends=["dist"])
register_target("resources", make_embedded_resources, depends=["exe"], default_build_script=True)
register_target("install", make_install, depends=["exe"], default=True)

resolve_targets()

PYOXIDIZER_VERSION = "0.16.2"
PYOXIDIZER_COMMIT = "e91995636f8deed0a7d8e1917f96a7dc17309b63"
