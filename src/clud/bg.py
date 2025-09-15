"""Background agent CLI for clud."""

import sys

from .cli import (
    ConfigError,
    DockerError,
    ValidationError,
    build_docker_image,
    check_docker_available,
    create_parser,
    get_api_key,
    handle_login,
    launch_container_shell,
    pull_latest_image,
    run_ui_container,
    validate_path,
)


def main(args: list[str] | None = None) -> int:
    """Main entry point for clud background agent."""
    # clud CLI is only used outside containers to launch development environments
    # Inside containers, 'clud' is a bash alias to 'claude code --dangerously-skip-permissions'

    parser = create_parser()
    parsed_args = parser.parse_args(args)

    # Handle conflicting firewall options
    if parsed_args.no_firewall:
        parsed_args.enable_firewall = False

    try:
        # Handle login command first (doesn't need Docker)
        if parsed_args.login:
            return handle_login()

        # Check Docker availability first for all modes that need Docker
        if not check_docker_available():
            raise DockerError("Docker is not available or not running")

        # Handle update mode - always pull from remote registry
        if parsed_args.update:
            print("Updating clud runtime...")

            # Determine which image to pull
            # User specified a custom image, otherwise use the standard Claude Code image from Docker Hub
            # This ensures we always pull from remote, not build locally
            image_to_pull = parsed_args.image or "niteris/clud:latest"

            print(f"Pulling the latest version of {image_to_pull}...")

            if pull_latest_image(image_to_pull):
                print(f"Successfully updated {image_to_pull}")
                print("You can now run 'clud' to use the updated runtime.")

                # If they pulled a non-default image, remind them to use --image flag
                if image_to_pull != "niteris/clud:latest" and not parsed_args.image:
                    print(f"Note: To use this image, run: clud --image {image_to_pull}")

                return 0
            else:
                print(f"Failed to update {image_to_pull}", file=sys.stderr)
                print("Please check your internet connection and Docker configuration.")
                return 1

        # Handle build-only mode
        if parsed_args.just_build:
            print("Building Docker image...")
            if build_docker_image(getattr(parsed_args, "build_dockerfile", None)):
                print("Docker image built successfully!")
                return 0
            else:
                print("Failed to build Docker image", file=sys.stderr)
                return 1

        # Force build if requested
        if parsed_args.build:
            print("Building Docker image...")
            if not build_docker_image(getattr(parsed_args, "build_dockerfile", None)):
                print("Failed to build Docker image", file=sys.stderr)
                return 1
            parsed_args._image_built = True

        # Route to different modes
        if parsed_args.ui:
            # UI mode - launch code-server container
            project_path = validate_path(parsed_args.path)
            api_key = get_api_key(parsed_args)

            return run_ui_container(parsed_args, project_path, api_key)
        else:
            # Default mode - launch container with interactive shell
            return launch_container_shell(parsed_args)

    except ValidationError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 2
    except DockerError as e:
        print(f"Docker error: {e}", file=sys.stderr)
        return 3
    except ConfigError as e:
        print(f"Configuration error: {e}", file=sys.stderr)
        return 4
    except KeyboardInterrupt:
        print("\nOperation cancelled.", file=sys.stderr)
        return 2
    except Exception as e:
        print(f"Unexpected error: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
