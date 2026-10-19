// SPDX-License-Identifier: MIT
#include "backend.h"

#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusInterface>
#include <QDBusMessage>
#include <QDBusReply>
#include <QDBusVariant>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocalSocket>

static const QString kSock = QStringLiteral("/run/tuxedo-control.sock");
static const QString kPpdService = QStringLiteral("net.hadess.PowerProfiles");
static const QString kPpdPath = QStringLiteral("/net/hadess/PowerProfiles");
static const QString kPpdIface = QStringLiteral("net.hadess.PowerProfiles");
static const QString kPropsIface = QStringLiteral("org.freedesktop.DBus.Properties");

Backend::Backend(QObject *parent)
    : QObject(parent)
{
    connect(&m_timer, &QTimer::timeout, this, &Backend::refresh);
    m_timer.start(1500);

    QDBusConnection::systemBus().connect(kPpdService, kPpdPath, kPropsIface, QStringLiteral("PropertiesChanged"), this,
                                         SLOT(onPowerPropertiesChanged(QString, QVariantMap, QStringList)));
    readPowerProfile();
    refresh();
}

QString Backend::send(const QString &cmd)
{
    QLocalSocket s;
    s.connectToServer(kSock);
    if (!s.waitForConnected(300))
        return {};
    s.write(cmd.toUtf8() + '\n');
    if (!s.waitForBytesWritten(300))
        return {};
    QByteArray line;
    while (!line.endsWith('\n')) {
        if (!s.waitForReadyRead(1000))
            break;
        line += s.readAll();
    }
    return QString::fromUtf8(line).trimmed();
}

void Backend::refresh()
{
    const QString st = send(QStringLiteral("STATUS"));
    const auto doc = QJsonDocument::fromJson(st.toUtf8());
    const bool up = doc.isObject();
    m_status = up ? doc.object().toVariantMap() : QVariantMap{};
    m_up = up;
    Q_EMIT statusChanged();

    if (up) {
        const QByteArray raw = send(QStringLiteral("LISTPROFILES")).toUtf8();
        if (raw != m_profilesRaw) {
            const auto pdoc = QJsonDocument::fromJson(raw);
            if (pdoc.isObject()) {
                m_profilesRaw = raw;
                m_profiles = pdoc.object().toVariantMap();
                Q_EMIT profilesChanged();
            }
        }
    }
}

bool Backend::saveProfile(const QString &name, const QString &perf, const QVariantList &curve, const QVariant &kbd,
                          const QVariant &charge)
{
    QJsonArray pts;
    for (const QVariant &v : curve) {
        const QVariantList p = v.toList();
        if (p.size() == 2)
            pts.append(QJsonArray{p[0].toInt(), p[1].toInt()});
    }
    QJsonObject o{{QStringLiteral("name"), name}, {QStringLiteral("perf"), perf}, {QStringLiteral("curve"), pts}};
    if (kbd.isValid() && !kbd.isNull())
        o.insert(QStringLiteral("kbd"), kbd.toInt());
    if (charge.isValid() && !charge.isNull() && !charge.toString().isEmpty())
        o.insert(QStringLiteral("charge"), charge.toString());
    const bool ok = send(QStringLiteral("SAVEPROFILE ") + QString::fromUtf8(QJsonDocument(o).toJson(QJsonDocument::Compact)))
        == QStringLiteral("OK");
    refresh();
    return ok;
}

int Backend::safetyFloor(int temp) const
{
    // Mirrors profiles::safety_floor in the daemon; the daemon enforces it regardless.
    if (temp >= 90)
        return 80;
    if (temp >= 85)
        return 60;
    if (temp >= 80)
        return 45;
    if (temp >= 75)
        return 30;
    return 0;
}

void Backend::readPowerProfile()
{
    QDBusInterface props(kPpdService, kPpdPath, kPropsIface, QDBusConnection::systemBus());
    QDBusReply<QDBusVariant> active = props.call(QStringLiteral("Get"), kPpdIface, QStringLiteral("ActiveProfile"));
    QString power = active.isValid() ? active.value().variant().toString() : QString();

    QStringList powers;
    QDBusReply<QDBusVariant> list = props.call(QStringLiteral("Get"), kPpdIface, QStringLiteral("Profiles"));
    if (list.isValid()) {
        const QDBusArgument arg = list.value().variant().value<QDBusArgument>();
        arg.beginArray();
        while (!arg.atEnd()) {
            QVariantMap m;
            arg >> m;
            powers << m.value(QStringLiteral("Profile")).toString();
        }
        arg.endArray();
    }
    if (power != m_power || powers != m_powers) {
        m_power = power;
        m_powers = powers;
        Q_EMIT powerProfileChanged();
    }
}

void Backend::onPowerPropertiesChanged(const QString &, const QVariantMap &, const QStringList &)
{
    readPowerProfile();
}

bool Backend::setPowerProfile(const QString &profile)
{
    QDBusMessage msg = QDBusMessage::createMethodCall(kPpdService, kPpdPath, kPropsIface, QStringLiteral("Set"));
    msg << kPpdIface << QStringLiteral("ActiveProfile") << QVariant::fromValue(QDBusVariant(profile));
    msg.setInteractiveAuthorizationAllowed(true);
    const QDBusMessage reply = QDBusConnection::systemBus().call(msg, QDBus::Block, 30000);
    readPowerProfile();
    return reply.type() != QDBusMessage::ErrorMessage;
}
