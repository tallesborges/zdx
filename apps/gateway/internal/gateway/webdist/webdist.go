// Package webdist holds the built Mini App (apps/web/dist) for the gateway.
// A release build copies dist here and builds with `-tags webdist`
// (apps/gateway/build.sh); other builds embed nothing, and the gateway
// then needs --web-dist or refuses to start the Mini App.
package webdist
