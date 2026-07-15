@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "ACTION=%~1"
if "%ACTION%"=="" set "ACTION=start"

call :ensure_docker
if errorlevel 1 exit /b 1

if /I "%ACTION%"=="start" goto start
if /I "%ACTION%"=="restart" goto restart
if /I "%ACTION%"=="build" goto build
if /I "%ACTION%"=="stop" goto stop
if /I "%ACTION%"=="logs" goto logs
if /I "%ACTION%"=="status" goto status
if /I "%ACTION%"=="spotify-login" goto spotify_login
if /I "%ACTION%"=="spotify-status" goto spotify_status
if /I "%ACTION%"=="apple-music-status" goto apple_music_status
if /I "%ACTION%"=="apple-status" goto apple_music_status

echo CappyFM: unknown command "%ACTION%". Use: run.cmd [start^|restart^|build^|stop^|logs^|status^|spotify-login^|spotify-status^|apple-music-status] 1>&2
exit /b 1

:start
call :ensure_environment
if errorlevel 1 exit /b 1
call :check_spotify
call :check_apple_music
echo CappyFM: building and starting the bot and Lavalink...
docker compose up -d --build --force-recreate
if errorlevel 1 exit /b 1
docker compose ps
echo.
echo CappyFM is running. Use "run.cmd logs" to watch it or "run.cmd stop" to shut it down.
exit /b 0

:restart
call :ensure_environment
if errorlevel 1 exit /b 1
call :check_spotify
call :check_apple_music
echo CappyFM: rebuilding and restarting...
docker compose up -d --build --force-recreate
if errorlevel 1 exit /b 1
docker compose ps
exit /b 0

:build
call :ensure_environment
if errorlevel 1 exit /b 1
echo CappyFM: building the bot and Lavalink images...
docker compose build
if errorlevel 1 exit /b 1
echo CappyFM: build complete. Run "run.cmd start" when you are ready.
exit /b 0

:stop
docker compose down
if errorlevel 1 exit /b 1
echo CappyFM is stopped. Persistent data remains in .\data.
exit /b 0

:logs
docker compose logs --tail=150 -f bot lavalink
exit /b %errorlevel%

:status
docker compose ps
exit /b %errorlevel%

:spotify_login
call :ensure_environment
if errorlevel 1 exit /b 1
echo CappyFM: building the one-time Spotify authorization helper...
echo CappyFM: make sure http://127.0.0.1:8888/callback is listed in your Spotify app's redirect URIs.
docker compose build bot
if errorlevel 1 exit /b 1
docker compose run --rm --no-deps -p 127.0.0.1:8888:8888 -e SPOTIFY_REDIRECT_URI=http://127.0.0.1:8888/callback -e SPOTIFY_REFRESH_TOKEN_FILE=/app/data/spotify-refresh-token bot cappy-bot --spotify-login
if errorlevel 1 exit /b 1
echo CappyFM: restarting the bot with Spotify playlist access...
docker compose up -d --build bot
exit /b %errorlevel%

:spotify_status
call :ensure_environment
if errorlevel 1 exit /b 1
call :check_spotify
exit /b %errorlevel%

:apple_music_status
call :ensure_environment
if errorlevel 1 exit /b 1
call :check_apple_music
exit /b %errorlevel%

:check_apple_music
findstr /R /C:"^[ ]*APPLE_MUSIC_API_TOKEN=.[^ ]*" .env >nul
if errorlevel 1 (
  echo CappyFM: Apple Music link support is disabled. Add APPLE_MUSIC_API_TOKEN to .env.
  exit /b 1
)
echo CappyFM: Apple Music song, album, and playlist link support is configured.
exit /b 0

:check_spotify
set "SPOTIFY_MISSING=0"
findstr /R /C:"^[ ]*SPOTIFY_CLIENT_ID=.[^ ]*" .env >nul
if errorlevel 1 (
  echo CappyFM: Spotify client ID is missing from .env.
  set "SPOTIFY_MISSING=1"
)
findstr /R /C:"^[ ]*SPOTIFY_CLIENT_SECRET=.[^ ]*" .env >nul
if errorlevel 1 (
  echo CappyFM: Spotify client secret is missing from .env.
  set "SPOTIFY_MISSING=1"
)
if exist data\spotify-refresh-token goto spotify_token_ready
findstr /R /C:"^[ ]*SPOTIFY_REFRESH_TOKEN=.[^ ]*" .env >nul
if not errorlevel 1 goto spotify_token_ready
echo CappyFM: Spotify playlist authorization is missing. Run "run.cmd spotify-login".
set "SPOTIFY_MISSING=1"
:spotify_token_ready
if "%SPOTIFY_MISSING%"=="0" echo CappyFM: Spotify credentials and playlist authorization are configured.
exit /b %SPOTIFY_MISSING%

:ensure_docker
where docker >nul 2>&1
if errorlevel 1 (
  echo CappyFM: Docker is not installed. Install Docker Desktop first. 1>&2
  exit /b 1
)

docker compose version >nul 2>&1
if errorlevel 1 (
  echo CappyFM: Docker Compose v2 is not available. 1>&2
  exit /b 1
)

docker info >nul 2>&1
if not errorlevel 1 exit /b 0

set "DOCKER_DESKTOP=%ProgramFiles%\Docker\Docker\Docker Desktop.exe"
if not exist "%DOCKER_DESKTOP%" (
  echo CappyFM: Docker Desktop is installed but is not running. Start it, then run this command again. 1>&2
  exit /b 1
)

echo CappyFM: starting Docker Desktop...
start "" "%DOCKER_DESKTOP%"
for /L %%I in (1,1,60) do (
  docker info >nul 2>&1
  if not errorlevel 1 goto docker_ready
  timeout /t 2 /nobreak >nul
)
echo CappyFM: Docker Desktop did not become ready within two minutes. 1>&2
exit /b 1

:docker_ready
echo CappyFM: Docker is ready.
exit /b 0

:ensure_environment
if not exist .env (
  copy /Y .env.example .env >nul
  echo CappyFM: created .env from .env.example. Fill in your secrets, then run run.cmd again. 1>&2
  exit /b 1
)

findstr /R /C:"^[ ]*DISCORD_TOKEN=[ ]*$" .env >nul
if not errorlevel 1 (
  echo CappyFM: DISCORD_TOKEN is empty in .env. 1>&2
  exit /b 1
)

if not exist data mkdir data
if not exist plugins mkdir plugins
exit /b 0
