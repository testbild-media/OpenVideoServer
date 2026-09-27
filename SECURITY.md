# Security

## Reporting something

Use GitHub's **Report a vulnerability** button on the Security tab of this
repository. That opens a private advisory rather than a public issue, which
means a fix can go out before the details do.

Please do not open a public issue for anything that would let somebody take
over a running show.

## What this server assumes

**It trusts the network it is on.** That is a deliberate decision by the
project owner, not an oversight, and it shapes everything below.

A media server for live events sits on a production LAN behind a switch, with a
lighting console and one or two clients on it and nothing else. It is often
built the afternoon of the show, by somebody who is not a system
administrator, on a network with no DNS and no certificate authority. A design
that demanded TLS on that network would mostly produce servers with TLS turned
off.

So:

- **There is no TLS.** The client, the web pages and the API all speak plain
  HTTP. **Passwords cross the network in clear**, and so does everything else.
  Anyone who can see the wire can see a sign-in. Put this server on a network
  you control, and do not put it on one you share with an audience.
- **The status page needs no sign-in.** Anybody who can reach port 80 can see
  the machine's name, its version, what show is loaded, how hard it is working
  and whether it still needs setting up. That is the point of a status page,
  and it is also a list of things an attacker would otherwise have to guess.
  Administration is behind a sign-in; looking is not.
- **Requests from the machine itself are trusted.** The rack touchscreen runs
  on the server and has no keyboard, so it has no sign-in either — anybody
  standing at the machine can operate it. Physical access to the rack is the
  boundary there, as it is for the power switch.
- **Art-Net and sACN have no authentication at all.** Neither protocol has any.
  A console on the LAN can take the show, which is exactly what a console is
  for.

## What it does not assume

- Passwords are stored with Argon2, never in plain text and never recoverable.
  A stolen database does not hand over the passwords in it.
- Sessions are random tokens in `HttpOnly`, `SameSite=Strict` cookies, so a
  page on another site cannot use one and script on the page cannot read one.
- Sign-in does not say whether it was the name or the password that was wrong,
  because the difference enumerates accounts.
- The download endpoint refuses any name carrying a path separator, `..` or a
  leading dot, rather than trying to clean it up. A request it does not
  understand is refused, not repaired.
- The services run as three unprivileged system accounts under a systemd
  sandbox: read-only system, no home directories, and only the address families
  and capabilities they actually need.

## If you need more than that

Put it behind something. A reverse proxy with a certificate, a VPN, or a VLAN
that the audience network cannot reach are all better answers than anything
this server could do for itself, and none of them require it to change.
