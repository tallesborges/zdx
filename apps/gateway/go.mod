module github.com/tallesborges/zdx/apps/gateway

go 1.27

toolchain go1.27.4

require (
	github.com/tallesborges/zdx/apps/shared v0.0.0
	github.com/yuin/goldmark/v2 v2.1.16
)

replace github.com/tallesborges/zdx/apps/shared => ../shared
