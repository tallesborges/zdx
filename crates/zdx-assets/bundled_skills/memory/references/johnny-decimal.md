# Johnny Decimal filing reference

Load this reference when you need to choose where a memory note belongs or when the user asks to organize notes following the existing Johnny Decimal structure.

This is a filing reference, not a mandate to reorganize memory. Prefer the structure that already exists on disk.

## Core rules

- Follow existing folders before proposing new ones
- Do not invent new Johnny Decimal codes without explicit approval
- Prefer updating an existing note over creating a new one
- Prefer small append/merge changes over broad restructuring
- Do not move or merge top-level areas without explicit confirmation
- If the right destination is unclear, inspect nearby folders and notes first

Top-level areas currently in use:

- `10-19 Life Admin`
- `20-29 Development & Tech`
- `30-39 Work`

## How to choose a destination

1. Check the memory index for a direct pointer
2. Search the relevant area for matching notes or sibling topics
3. Reuse an existing note when possible
4. If you must create a note, match the folder's current naming pattern
5. If a new folder seems necessary, propose the exact folder name/code and wait for approval

Useful discovery patterns:

```text
glob path:"$ZDX_MEMORY_ROOT/Notes" pattern:"**/*.md"
grep file_path:"$ZDX_MEMORY_ROOT/Notes" glob:"**/*.md" pattern:"project bravo|parity|zdx" case_insensitive:true
```

## Johnny Decimal naming patterns

Patterns that may exist in structured areas:

- `xx.00 Name/xx.00 Name.md` for an area folder + index note
- `xx.01+` sibling folders or notes for subareas
- date-prefixed note names inside a Johnny Decimal folder when chronology matters

Treat these as local conventions, not rigid requirements. Match what is already present.

## `10-19 Life Admin` reference layout

Important: numbered folders such as `11.xx` through `15.xx` live flat inside `10-19 Life Admin/`. The group labels below are for readability only.

Before creating a new note in this area, inspect the actual folder names already present.

### Personal (`11.xx`)

- `11.00 ■ System management`
- `11.01 Inbox 📥`
- `11.02 System manual 📙`
- `11.09 Archive 📦`
- `11.10 ■ Personal records 🗂️`
- `11.11 Birth certificate & proof of name`
- `11.12 Passports, residency, & citizenship`
- `11.13 Identity cards`
- `11.14 Licenses`
- `11.15 Voter registration & elections`
- `11.16 Legal documents & certificates`
- `11.17 Academic records & qualifications`
- `11.20 ■ Physical health & wellbeing 🫀`
- `11.21 Health insurance & claims`
- `11.22 Health records & registrations`
- `11.23 Primary care`
- `11.24 Eyes, ears, & teeth`
- `11.25 Immunity`
- `11.26 Physical therapy`
- `11.27 Fitness, nutrition, sleep, & other pro-active wellbeing`
- `11.28 Reproductive health`
- `11.29 Surgical & specialist care`
- `11.30 ■ Mental health & wellbeing 🧠`
- `11.31 Psychologist, psychiatrist, & counselling`
- `11.32 My thoughts, journalling, diaries, & other writing`
- `11.33 Spiritual`
- `11.34 Habits, routines, & planning`
- `11.35 Brain training`
- `11.40 ■ Family 💑`
- `11.41 My partner`
- `11.42 My kids`
- `11.43 My family`
- `11.44 Dating & relationships`
- `11.45 Celebrations & gifting`
- `11.46 Letters, cards, & mementos`
- `11.50 ■ Friends, clubs, & organisations 🏑`
- `11.51 My friends`
- `11.52 Groups, clubs, & volunteering`
- `11.53 Official correspondence`
- `11.60 ■ Pets & other animals 🐓`
- `11.61 Pet health insurance & claims`
- `11.62 Pet health records & registrations`
- `11.70 ■ My brilliant career 🧑‍🍳`
- `11.71 My sales pitch`
- `11.72 My jobs past, present, & future`
- `11.73 My side-hustles`
- `11.80 ■ Personal development & who I am 📚`
- `11.81 Goals & dreams`
- `11.82 Hobbies & learning`
- `11.83 My library`

### Home (`12.xx`)

- `12.00 ■ System management`
- `12.01 Inbox 📥`
- `12.09 Archive 📦`
- `12.10 ■ Home records 📄`
- `12.11 Official documents`
- `12.12 Home insurance & claims`
- `12.13 Moving`
- `12.14 Inventory`
- `12.15 My home's user manual`
- `12.16 Appliances, tools, & gadgets`
- `12.17 Rates, taxes, & fees`
- `12.20 ■ Home services & health 🛠️`
- `12.21 Electricity, gas, & water`
- `12.22 Internet, phone, TV, & cable`
- `12.23 All other utilities & services`
- `12.24 Repairs, maintenance, & upkeep`
- `12.25 Renovation & improvements`
- `12.26 Cleaning Services & housekeeping`
- `12.30 ■ Getting around 🚲`
- `12.31 Motor vehicle purchase, leasing, & rental`
- `12.33 Mechanics, repairs, & maintenance`
- `12.34 Permits & tolls`
- `12.35 Bicycles & scooters`
- `12.36 Public transport`
- `12.40 ■ My kitchen & garden 🪴`
- `12.41 Indoor plants`
- `12.42 Outdoor plants`
- `12.43 Growing herbs, vegetables, & fruit`
- `12.50 ■ Housemates, neighbours, & the neighbourhood ☕️`
- `12.51 Housemates`
- `12.52 Neighbours`
- `12.53 The neighbourhood`

### Money (`13.xx`)

- `13.00 ■ System management`
- `13.01 Inbox 📥`
- `13.09 Archive 📦`
- `13.10 ■ Earned 🤑`
- `13.11 Payslips, invoices, & remittance`
- `13.12 Expenses & claims`
- `13.13 Government services`
- `13.14 Gifts, prizes, inheritance, & windfalls`
- `13.15 Selling my stuff`
- `13.20 ■ Saved 📈`
- `13.21 Budgets & planning`
- `13.22 Bank accounts`
- `13.23 Investments & assets`
- `13.24 Pension`
- `13.30 ■ Owed 💸`
- `13.31 Credit cards`
- `13.32 Mortgage`
- `13.33 Personal loans`
- `13.34 Tax returns & accounting`
- `13.40 ■ Spent & sent 🛍️`
- `13.41 Purchase receipts`
- `13.43 Payment services`
- `13.44 Money transfer services`
- `13.50 ■ Financial administration 📔`
- `13.51 My credit rating`

### Tech (`14.xx`)

- `14.00 ■ System management`
- `14.01 Inbox 📥`
- `14.09 Archive 📦`
- `14.10 ■ Computers & other devices 🖥️`
- `14.11 My computers & servers`
- `14.12 My mobile devices`
- `14.13 My wi-fi & network devices`
- `14.14 My data storage & backups`
- `14.20 ■ Software & accounts 🔑`
- `14.21 My emergency recovery kit 🚨`
- `14.22 Software, licenses, & downloads`
- `14.23 Email accounts`
- `14.24 Social media accounts`
- `14.25 Domains & hosting`
- `14.26 All other accounts`
- `14.30 ■ My online presence 🌏`
- `14.31 My blog`

### Lifestyle & travel (`15.xx`)

- `15.00 ■ System management`
- `15.01 Inbox 📥`
- `15.09 Archive 📦`
- `15.10 ■ Inspiration & history 💭`
- `15.11 Places I've been, or want to go`
- `15.12 Places I'd like to eat or drink`
- `15.20 ■ Administration & checklists ✅`
- `15.21 Important documents & lists`
- `15.22 Going-away checklists`
- `15.24 Loyalty programs`
- `15.30 ■ Events 🍿`
- `15.31 Eating out`
- `15.32 Music`
- `15.33 Movies`
- `15.34 The arts`
- `15.35 Sport`
- `15.36 Fairs & shows`
- `15.37 Conferences & expos`
- `15.40 ■ Short or routine trips 🚉`
- `15.41 All short trips`
- `15.50 ■ Longer trips 🛫`
- `15.51 Longer trips from the past`
- `15.52 Lucerne 2025-03`

## Reorganization guardrails

- For large reorganizations or mass moves, propose a plan first
- If a note has a related archive note, prefer archiving removed/completed items instead of deleting them silently
- After filing edits, summarize the note path(s) changed and the reason for the placement