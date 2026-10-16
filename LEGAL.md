# Mentions légales

## Non-affiliation

Ce projet est indépendant. Il n'est ni développé, ni approuvé, ni soutenu par NZXT, Inc. « NZXT », « Kraken » et « CAM », ainsi que les logos et icônes associés, sont des marques ou des œuvres protégées appartenant à leurs titulaires.

## Interopérabilité et sources

NZXT Lite pilote une Kraken Z avec un code écrit indépendamment, dans un seul but : **l'interopérabilité**, c'est-à-dire permettre à un programme indépendant de dialoguer avec le matériel de l'utilisateur.

- Le protocole USB de la Kraken s'appuie sur la documentation publique du projet open source [liquidctl](https://github.com/liquidctl/liquidctl) (pilote `kraken3.py`). Aucun code de liquidctl n'est copié ; seuls les formats de messages sont réimplémentés.
- Les valeurs des préréglages « Silencieux » / « Performance » et le format du profil de l'utilisateur ont été relevés par une analyse **statique** de NZXT CAM installé sur la machine de l'auteur (lecture de son code JavaScript et de ses fichiers de configuration locaux).
- Aucun code, firmware ni fichier de NZXT CAM n'est redistribué dans ce dépôt, à l'exception de l'icône mentionnée ci-dessous.
- Aucune protection technique (chiffrement, DRM, signature de firmware) n'est contournée. Le firmware de la Kraken n'est jamais modifié.
- Le capteur CPU utilise le pilote WinRing0 seulement s'il est déjà installé sur la machine ; ce dépôt ne fournit ni n'installe aucun pilote.

Dans l'Union européenne, la décomposition d'un programme à des fins d'interopérabilité est encadrée par l'article 6 de la directive 2009/24/CE (en France, article L122-6-1 IV du Code de la propriété intellectuelle). Ce texte n'est cité qu'à titre d'information et ne constitue pas un avis juridique.

## Contenus tiers inclus

- `assets/icon.ico` : icône de NZXT CAM, extraite de l'exécutable officiel pour que l'application reste reconnaissable. Elle reste la propriété de NZXT, Inc. et n'est pas couverte par la licence du code.

**Si vous êtes titulaire de droits sur l'un de ces éléments (icône, marque, documentation du protocole) et souhaitez son retrait, ouvrez une issue sur le dépôt ou contactez l'auteur : il sera retiré rapidement.**

## Usage et responsabilité

- Usage personnel, sans but commercial. Aucune donnée n'est collectée ni envoyée : le logiciel ne communique qu'avec le matériel branché sur la machine, jamais avec Internet.
- Piloter la pompe et les ventilateurs avec un logiciel tiers se fait à vos risques. Par sécurité, la consigne est forcée à 100 % à 59 °C de liquide et la pompe ne descend jamais sous 20 %.
- Le logiciel est fourni « tel quel », sans garantie d'aucune sorte. L'auteur n'est pas responsable des dommages matériels, pertes de données ou problèmes de garantie liés à son utilisation.
