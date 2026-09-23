# NAME

bootc-fetch-apply-updates.service

# DESCRIPTION

This service causes `bootc` to perform the following steps:

- Check the source registry for an updated container image
- If one is found, download it
- Reboot

The reboot honors systemd inhibitor locks: if a process holds a
`block` mode `shutdown` inhibitor (see `systemd-inhibit(1)`), or a
non-root user is logged in (on a local terminal, a graphical session,
or via SSH with a terminal), the reboot is refused and the service
fails. The update remains staged and will be applied on the next
reboot, or on the next run of this service. This matches
`systemctl reboot --check-inhibitors=yes`; to reboot anyway, use e.g.
`systemctl reboot --check-inhibitors=no`.

This service also comes with a companion `bootc-fetch-apply-updates.timer`
systemd unit.  The current default systemd timer shipped in the upstream
project is enabled for daily updates.

However, it is fully expected that different operating systems
and distributions choose different defaults.

# CUSTOMIZING UPDATES

Note that all three of these steps can be decoupled; they
are:

- `bootc upgrade --check`
- `bootc upgrade`
- `bootc upgrade --apply`

# SEE ALSO

**bootc(1)**

# VERSION

<!-- VERSION PLACEHOLDER -->