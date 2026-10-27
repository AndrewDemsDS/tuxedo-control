// SPDX-License-Identifier: MIT
#include <KAboutData>
#include <KLocalizedQmlContext>
#include <KLocalizedString>
#include <QApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QtQml>

int main(int argc, char *argv[])
{
    QApplication app(argc, argv);
    KLocalizedString::setApplicationDomain("tuxedo-control-kde");
    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE"))
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));

    KAboutData about(QStringLiteral("tuxedo-control-kde"), i18n("TUXEDO Control"), QStringLiteral("0.3.1"),
                     i18n("Fan, EC profile, keyboard and charging control for TUXEDO Uniwill laptops"),
                     KAboutLicense::MIT);
    about.setDesktopFileName(QStringLiteral("org.andreasincode.tuxedocontrol"));
    about.setHomepage(QStringLiteral("https://github.com/AndrewDemsDS/tuxedo-control"));
    KAboutData::setApplicationData(about);
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("preferences-system-power-management")));

    QQmlApplicationEngine engine;
    KLocalization::setupLocalizedContext(&engine);
    engine.loadFromModule("org.andreasincode.tuxedocontrol", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;
    return app.exec();
}
